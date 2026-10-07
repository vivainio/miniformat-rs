//! Optional serde support (`--features serde`): deserialize straight from the
//! event stream into your types, without building a tree.
//!
//! Scalars are strings, so a number or bool field is parsed from its text
//! (`port: 5432` into a `u16`; `true`/`false` into a `bool`). An empty value
//! (`key:`) is `None` for an `Option` and `()` for the unit type.

use crate::{Error, Event, Reader};
use serde::de::{self, DeserializeOwned, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor};
use std::borrow::Cow;
use std::path::Path;

type R<T> = Result<T, Error>;

impl de::Error for Error {
    fn custom<T: std::fmt::Display>(msg: T) -> Self {
        Error::new(msg.to_string(), None, None, None)
    }
}

/// Deserialize a document. `base` is the directory `#+include` paths are
/// relative to. Borrowed fields (`&str`, `Cow<str>`) work for plain scalars.
pub fn from_str<'de, T: de::Deserialize<'de>>(text: &'de str, base: Option<&Path>) -> R<T> {
    let mut de = De {
        r: Reader::new(text, base)?,
        peeked: None,
    };
    let v = T::deserialize(&mut de)?;
    // run to the end so the whole document is validated, as `loads` does
    if let Some(ev) = de.peeked.take().map(Ok).or_else(|| de.r.next()) {
        ev?;
        return Err(de::Error::custom("unexpected content after the document"));
    }
    Ok(v)
}

/// Read and deserialize a file; includes are relative to its directory.
pub fn from_path<T: DeserializeOwned>(path: impl AsRef<Path>) -> R<T> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).map_err(<Error as de::Error>::custom)?;
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    from_str(&text, abs.parent())
}

struct De<'de> {
    r: Reader<'de>,
    peeked: Option<Event<'de>>,
}

impl<'de> De<'de> {
    fn next(&mut self) -> R<Event<'de>> {
        match self.peeked.take() {
            Some(e) => Ok(e),
            None => self
                .r
                .next()
                .unwrap_or_else(|| Err(de::Error::custom("unexpected end of document"))),
        }
    }

    fn peek(&mut self) -> R<&Event<'de>> {
        if self.peeked.is_none() {
            self.peeked = Some(self.next()?);
        }
        Ok(self.peeked.as_ref().unwrap())
    }

    fn skip(&mut self, first: Event<'de>) -> R<()> {
        let mut depth = matches!(first, Event::MapStart | Event::ListStart) as usize;
        while depth > 0 {
            match self.next()? {
                Event::MapStart | Event::ListStart => depth += 1,
                Event::MapEnd | Event::ListEnd => depth -= 1,
                _ => {}
            }
        }
        Ok(())
    }

    fn expect_end(&mut self) -> R<()> {
        match self.next()? {
            Event::MapEnd => Ok(()),
            _ => Err(de::Error::custom("an enum variant is a map with a single key")),
        }
    }
}

/// A scalar (or key) as a deserializer: parses itself on demand.
struct Scalar<'de>(Cow<'de, str>);

impl<'de> Scalar<'de> {
    fn parse<T: std::str::FromStr>(&self, what: &str) -> R<T> {
        self.0
            .parse()
            .map_err(|_| de::Error::custom(format!("invalid {what}: {:?}", self.0)))
    }
}

macro_rules! scalar_num {
    ($($method:ident $visit:ident $ty:ty),* $(,)?) => {$(
        fn $method<V: Visitor<'de>>(self, v: V) -> R<V::Value> {
            v.$visit(self.parse::<$ty>(stringify!($ty))?)
        }
    )*};
}

impl<'de> de::Deserializer<'de> for Scalar<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, v: V) -> R<V::Value> {
        match self.0 {
            Cow::Borrowed(s) => v.visit_borrowed_str(s),
            Cow::Owned(s) => v.visit_string(s),
        }
    }

    scalar_num! {
        deserialize_i8 visit_i8 i8, deserialize_i16 visit_i16 i16, deserialize_i32 visit_i32 i32,
        deserialize_i64 visit_i64 i64, deserialize_i128 visit_i128 i128,
        deserialize_u8 visit_u8 u8, deserialize_u16 visit_u16 u16, deserialize_u32 visit_u32 u32,
        deserialize_u64 visit_u64 u64, deserialize_u128 visit_u128 u128,
        deserialize_f32 visit_f32 f32, deserialize_f64 visit_f64 f64,
    }

    fn deserialize_bool<V: Visitor<'de>>(self, v: V) -> R<V::Value> {
        match &*self.0 {
            "true" => v.visit_bool(true),
            "false" => v.visit_bool(false),
            s => Err(de::Error::custom(format!(
                "invalid bool: {s:?} (expected true or false)"
            ))),
        }
    }

    fn deserialize_char<V: Visitor<'de>>(self, v: V) -> R<V::Value> {
        let mut it = self.0.chars();
        match (it.next(), it.next()) {
            (Some(c), None) => v.visit_char(c),
            _ => Err(de::Error::custom(format!("invalid char: {:?}", self.0))),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, v: V) -> R<V::Value> {
        if self.0.is_empty() {
            v.visit_none()
        } else {
            v.visit_some(self)
        }
    }

    fn deserialize_unit<V: Visitor<'de>>(self, v: V) -> R<V::Value> {
        if self.0.is_empty() {
            v.visit_unit()
        } else {
            Err(de::Error::custom(format!(
                "invalid unit: {:?} (expected an empty value)",
                self.0
            )))
        }
    }

    fn deserialize_enum<V: Visitor<'de>>(self, _: &'static str, _: &'static [&'static str], v: V) -> R<V::Value> {
        match self.0 {
            Cow::Borrowed(s) => v.visit_enum(de::value::BorrowedStrDeserializer::<Error>::new(s)),
            Cow::Owned(s) => v.visit_enum(de::value::StringDeserializer::<Error>::new(s)),
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(self, _: &'static str, v: V) -> R<V::Value> {
        v.visit_newtype_struct(self)
    }

    serde::forward_to_deserialize_any! {
        str string bytes byte_buf unit_struct seq tuple tuple_struct map struct identifier ignored_any
    }
}

macro_rules! typed_scalar {
    ($($method:ident),* $(,)?) => {$(
        fn $method<V: Visitor<'de>>(self, v: V) -> R<V::Value> {
            match self.next()? {
                Event::Scalar(s) => Scalar(s).$method(v),
                _ => Err(de::Error::custom(concat!("expected a scalar for ", stringify!($method)))),
            }
        }
    )*};
}

impl<'de> de::Deserializer<'de> for &mut De<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, v: V) -> R<V::Value> {
        match self.next()? {
            Event::Scalar(s) => Scalar(s).deserialize_any(v),
            Event::MapStart => {
                let mut acc = Entries { de: self, done: false };
                let out = v.visit_map(&mut acc)?;
                acc.finish()?;
                Ok(out)
            }
            Event::ListStart => {
                let mut acc = Items { de: self, done: false };
                let out = v.visit_seq(&mut acc)?;
                acc.finish()?;
                Ok(out)
            }
            _ => Err(de::Error::custom("unexpected event")),
        }
    }

    typed_scalar! {
        deserialize_bool, deserialize_char,
        deserialize_i8, deserialize_i16, deserialize_i32, deserialize_i64, deserialize_i128,
        deserialize_u8, deserialize_u16, deserialize_u32, deserialize_u64, deserialize_u128,
        deserialize_f32, deserialize_f64, deserialize_unit,
    }

    fn deserialize_option<V: Visitor<'de>>(self, v: V) -> R<V::Value> {
        if matches!(self.peek()?, Event::Scalar(s) if s.is_empty()) {
            self.next()?;
            v.visit_none()
        } else {
            v.visit_some(self)
        }
    }

    fn deserialize_enum<V: Visitor<'de>>(self, _: &'static str, _: &'static [&'static str], v: V) -> R<V::Value> {
        match self.next()? {
            Event::Scalar(s) => Scalar(s).deserialize_enum("", &[], v),
            Event::MapStart => v.visit_enum(Variant { de: self }),
            _ => Err(de::Error::custom("an enum is a string, or a map with a single key")),
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(self, _: &'static str, v: V) -> R<V::Value> {
        v.visit_newtype_struct(self)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, v: V) -> R<V::Value> {
        let first = self.next()?;
        self.skip(first)?;
        v.visit_unit()
    }

    serde::forward_to_deserialize_any! {
        str string bytes byte_buf unit_struct seq tuple tuple_struct map struct identifier
    }
}

struct Items<'a, 'de> {
    de: &'a mut De<'de>,
    done: bool,
}

impl Items<'_, '_> {
    /// A visitor may stop early (a tuple reads exactly its length): the list must be over.
    fn finish(self) -> R<()> {
        if !self.done && !matches!(self.de.next()?, Event::ListEnd) {
            return Err(de::Error::custom("more list items than expected"));
        }
        Ok(())
    }
}

impl<'de> SeqAccess<'de> for &mut Items<'_, 'de> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(&mut self, seed: T) -> R<Option<T::Value>> {
        if matches!(self.de.peek()?, Event::ListEnd) {
            self.de.next()?;
            self.done = true;
            return Ok(None);
        }
        seed.deserialize(&mut *self.de).map(Some)
    }
}

struct Entries<'a, 'de> {
    de: &'a mut De<'de>,
    done: bool,
}

impl Entries<'_, '_> {
    fn finish(self) -> R<()> {
        if !self.done && !matches!(self.de.next()?, Event::MapEnd) {
            return Err(de::Error::custom("more map entries than expected"));
        }
        Ok(())
    }
}

impl<'de> MapAccess<'de> for &mut Entries<'_, 'de> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(&mut self, seed: K) -> R<Option<K::Value>> {
        match self.de.next()? {
            Event::MapEnd => {
                self.done = true;
                Ok(None)
            }
            Event::Key(k) => seed.deserialize(Scalar(k)).map(Some),
            _ => Err(de::Error::custom("expected a key")),
        }
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> R<V::Value> {
        seed.deserialize(&mut *self.de)
    }
}

/// `{ Variant: value }`: the map has started; its single key names the variant.
struct Variant<'a, 'de> {
    de: &'a mut De<'de>,
}

impl<'a, 'de> EnumAccess<'de> for Variant<'a, 'de> {
    type Error = Error;
    type Variant = Self;

    fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> R<(V::Value, Self)> {
        match self.de.next()? {
            Event::Key(k) => Ok((seed.deserialize(Scalar(k))?, self)),
            _ => Err(de::Error::custom("expected the variant name as a key")),
        }
    }
}

impl<'de> VariantAccess<'de> for Variant<'_, 'de> {
    type Error = Error;

    fn unit_variant(self) -> R<()> {
        match self.de.next()? {
            Event::Scalar(s) if s.is_empty() => self.de.expect_end(),
            _ => Err(de::Error::custom("a unit variant has an empty value")),
        }
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> R<T::Value> {
        let v = seed.deserialize(&mut *self.de)?;
        self.de.expect_end()?;
        Ok(v)
    }

    fn tuple_variant<V: Visitor<'de>>(self, _len: usize, v: V) -> R<V::Value> {
        let out = de::Deserializer::deserialize_seq(&mut *self.de, v)?;
        self.de.expect_end()?;
        Ok(out)
    }

    fn struct_variant<V: Visitor<'de>>(self, _fields: &'static [&'static str], v: V) -> R<V::Value> {
        let out = de::Deserializer::deserialize_map(&mut *self.de, v)?;
        self.de.expect_end()?;
        Ok(out)
    }
}

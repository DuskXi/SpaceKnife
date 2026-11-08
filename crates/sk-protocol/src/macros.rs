//! Helper macros for the protocol's literal types.

/// A closed set of string literals (pydantic `Literal["a", "b"]`).
///
/// Serialises as the literal, rejects anything else, and describes itself inline
/// (`{"type": "string", "enum": [...]}`) so the generated TypeScript is `"a" | "b"`.
macro_rules! str_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident = $lit:literal ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
        $vis enum $name {
            $( $(#[$vmeta])* #[serde(rename = $lit)] $variant, )+
        }

        impl $name {
            /// Every value, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// The wire spelling.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $lit,)+
                }
            }

            /// Parse the wire spelling.
            #[allow(clippy::should_implement_trait)]
            pub fn from_str(s: &str) -> Option<Self> {
                match s {
                    $($lit => Some($name::$variant),)+
                    _ => None,
                }
            }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl schemars::JsonSchema for $name {
            fn inline_schema() -> bool {
                true
            }
            fn schema_name() -> ::std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }
            fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
                schemars::json_schema!({ "type": "string", "enum": [$($lit),+] })
            }
        }
    };
}

/// A field that always holds one string (pydantic `kind: Literal["flame"] = "flame"`): the
/// discriminator of a tagged union. Deserialising anything else fails.
macro_rules! const_str {
    ($(#[$meta:meta])* $vis:vis struct $name:ident = $lit:literal;) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
        $vis struct $name;

        impl $name {
            /// The wire spelling.
            pub const VALUE: &'static str = $lit;
        }

        impl serde::Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str($lit)
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = <::std::borrow::Cow<'de, str> as serde::Deserialize>::deserialize(d)?;
                if s == $lit {
                    Ok($name)
                } else {
                    Err(serde::de::Error::invalid_value(serde::de::Unexpected::Str(&s), &$lit))
                }
            }
        }

        impl schemars::JsonSchema for $name {
            fn inline_schema() -> bool {
                true
            }
            fn schema_name() -> ::std::borrow::Cow<'static, str> {
                stringify!($name).into()
            }
            fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
                schemars::json_schema!({ "type": "string", "const": $lit })
            }
        }
    };
}

pub(crate) use const_str;
pub(crate) use str_enum;

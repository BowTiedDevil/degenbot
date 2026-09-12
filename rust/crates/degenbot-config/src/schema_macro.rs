//! The `config_schema!` declaration macro.
//!
//! One invocation line per key produces the typed field (Rust type), the
//! `DEGENBOT_*` env mapping, the TOML path, the typed default, and the
//! rendered doc entry. This module is the ONLY place where that expansion
//! logic lives; see `schema::SCHEMA` for the declaration list itself.

/// The declarative schema expansion: generates the typed `BotConfig` tree,
/// section `Default` impls, the path-addressed `assign` setter, and the
/// `SCHEMA` registry — all from ONE invocation line per key.
#[macro_export]
macro_rules! config_schema {
    ( $( $sec:ident $Sec:ident {
        $( $(#[$fmeta:meta])* $f:ident [ $($kt:tt)+ ] = $def:expr ,
           env = $env:literal , def = $def_repr:literal , doc = $doc:literal ;
        )+
    } )+ ) => {
        /// The typed configuration tree. Every field is generated from the
        /// same declaration that produced `SCHEMA` — one site per key
        /// (see the crate docs for the parity + precedence contract).
        #[derive(Debug, Clone, PartialEq)]
        pub struct BotConfig { $( pub $sec: $Sec, )+ }

        $(
            #[doc = concat!("Configuration section `", stringify!($sec), "`.")]
            #[derive(Debug, Clone, PartialEq)]
            pub struct $Sec {
                $( pub $f: $crate::cfg_ty!( $($kt)+ ), )+
            }

            impl ::core::default::Default for $Sec {
                fn default() -> Self {
                    Self { $( $f: $def, )+ }
                }
            }
        )+

        impl ::core::default::Default for BotConfig {
            fn default() -> Self {
                Self { $( $sec: $Sec::default(), )+ }
            }
        }

        // Enum kinds declare their generated enum types alongside the keys.
        $( $( $crate::cfg_enum!( $($kt)+ ); )+ )+

        impl BotConfig {
            /// Assign ONE declared key from a raw string. The label in errors
            /// is the dotted TOML path.
            ///
            /// # Errors
            ///
            /// Returns the error text when `raw` does not parse into the
            /// declared kind.
            pub fn assign(
                &mut self,
                section: &str,
                field: &str,
                raw: &str,
            ) -> Result<(), String> {
                match (section, field) {
                    $(
                        $(
                            (::core::stringify!($sec), ::core::stringify!($f)) => {
                                {
                        self.$sec.$f = $crate::cfg_parse_single!($($kt)+, raw)
                            .map_err(|e| {
                                format!(
                                    "{}: {e}",
                                    concat!(stringify!($sec), ".", stringify!($f))
                                )
                            })?;
                        Ok(())
                    }
                            }
                        )+
                    )+
                    _ => Ok(()),
                }
            }
        }

        /// The self-describing key registry: one entry per declared key, in
        /// declaration order (which fixes doc + loader iteration order).
        pub const SCHEMA: &[$crate::schema::KeyDecl] = &[
            $(
                $(
                    $crate::schema::KeyDecl {
                        section: ::core::stringify!($sec),
                        field: ::core::stringify!($f),
                        env: $env,
                        toml_path: concat!(::core::stringify!($sec), ".", ::core::stringify!($f)),
                        kind: $crate::cfg_kind!( $($kt)+ ),
                        default_repr: $def_repr,
                        description: $doc,
                    },
                )+
            )+
        ];
    }
}

/// Field type from a kind token sequence.
/// Field type from a kind token sequence.
#[doc(hidden)]
#[macro_export]
macro_rules! cfg_ty {
    (bool) => { bool };
    (bool_not) => { bool };
    (ms) => { u64 };
    (string) => { String };
    (path) => { ::std::path::PathBuf };
    (usize) => { usize };
    (u64) => { u64 };
    (i64) => { i64 };
    (i32) => { i32 };
    (u128) => { u128 };
    (f64) => { f64 };
    (opt bool) => { Option<bool> };
    (opt bool_not) => { Option<bool> };
    (opt string) => { Option<String> };
    (opt path) => { Option<::std::path::PathBuf> };
    (opt usize) => { Option<usize> };
    (opt i32) => { Option<i32> };
    (opt u64) => { Option<u64> };
    (opt i64) => { Option<i64> };
    (opt f64) => { Option<f64> };
    (opt enum $e:ident $( $v:ident $( = $alias:literal )? )+) => { Option<$e> };
    (map $e:ident) => { ::std::collections::BTreeMap<::std::string::String, $e> };
    (enum $e:ident $( $v:ident $( = $alias:literal )? )+) => { $e };
}

/// Generate the enum type (+ case-insensitive `FromStr` with optional legacy
/// aliases, and lowercase `Display`) for enum kinds; nothing for others.
#[doc(hidden)]
#[macro_export]
macro_rules! cfg_enum {
    (opt enum $e:ident $( $v:ident $( = $alias:literal )? )+) => {
        $crate::cfg_enum!(enum $e $( $v $( = $alias )? )+);
    };
    (enum $e:ident $( $v:ident $( = $alias:literal )? )+) => {
        #[doc = concat!("Enum-valued config key generated at its declaration site (`", stringify!($e), "`).")]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $e { $( $v, )+ }

        impl ::core::fmt::Display for $e {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                let rendered = match self { $( Self::$v => stringify!($v), )+ };
                f.write_str(&rendered.to_ascii_lowercase())
            }
        }

        impl ::core::str::FromStr for $e {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                // Match canonical names case-insensitively with `-`/`_` folded,
                // plus any declared literal alias per variant.
                let want = s.trim().to_ascii_lowercase().replace('-', "_");
                let ok = if let Some(v) = Self::from_canonical(&want) { Some(v) } else { None };
                if let Some(v) = ok {
                    return Ok(v);
                }
                $(
                    $(
                        if want == $alias {
                            return Ok(Self::$v);
                        }
                    )?
                )+
                Err(format!(
                    "invalid {} value {:?} (expected one of: {})",
                    stringify!($e),
                    s,
                    concat!($( stringify!($v), " " ),+)
                ))
            }
        }

        impl $e {
            fn from_canonical(want: &str) -> Option<Self> {
                match want {
                    $( s if s == stringify!($v).to_ascii_lowercase() => Some(Self::$v), )+
                    _ => None,
                }
            }
        }
    };
    ($($other:tt)*) => {};
}

/// Base kind mapping (used for `ValueKind`).
#[doc(hidden)]
#[macro_export]
macro_rules! cfg_base {
    (bool) => {
        $crate::schema::BaseKind::Bool
    };
    (bool_not) => {
        $crate::schema::BaseKind::BoolInverted
    };
    (ms) => {
        $crate::schema::BaseKind::Ms
    };
    (string) => {
        $crate::schema::BaseKind::Str
    };
    (path) => {
        $crate::schema::BaseKind::Path
    };
    (usize) => {
        $crate::schema::BaseKind::Usize
    };
    (u64) => {
        $crate::schema::BaseKind::U64
    };
    (i64) => {
        $crate::schema::BaseKind::I64
    };
    (i32) => {
        $crate::schema::BaseKind::I32
    };
    (u128) => {
        $crate::schema::BaseKind::U128
    };
    (f64) => {
        $crate::schema::BaseKind::F64
    };
}

/// `ValueKind` from a kind token sequence (scalar, `opt <scalar>`, or
/// `enum <Name> [variants with optional legacy aliases]`).
#[doc(hidden)]
#[macro_export]
macro_rules! cfg_kind {
    (opt $b:ident) => {
        $crate::schema::ValueKind { base: $crate::cfg_base!($b), optional: true }
    };
    (opt enum $e:ident $( $v:ident $( = $alias:literal )? )+) => {
        $crate::schema::ValueKind {
            base: $crate::schema::BaseKind::Enum(stringify!($e), &[ $( stringify!($v) ),+ ]),
            optional: true,
        }
    };
    (map $e:ident) => {
        $crate::schema::ValueKind {
            base: $crate::schema::BaseKind::Map(stringify!($e)),
            optional: false,
        }
    };
    (enum $e:ident $( $v:ident $( = $alias:literal )? )+) => {
        $crate::schema::ValueKind {
            base: $crate::schema::BaseKind::Enum(stringify!($e), &[ $( stringify!($v) ),+ ]),
            optional: false,
        }
    };
    ($b:ident) => {
        $crate::schema::ValueKind { base: $crate::cfg_base!($b), optional: false }
    };
}

/// Parse one raw string into a scalar for the given kind. Result type is
/// inferred from the assignment target (except `bool`/`bool_not`/enums).
#[doc(hidden)]
#[macro_export]
macro_rules! cfg_parse_single {
    (bool, $raw:expr) => {
        $crate::parse_bool_flag($raw)
    };
    (bool_not, $raw:expr) => {
        $crate::parse_bool_flag($raw).map(|b| !b)
    };
    (enum $e:ident $( $v:ident $( = $alias:literal )? )+, $raw:expr) => {
        // The generated FromStr renders the candidate list on error.
        <$e as ::core::str::FromStr>::from_str($raw)
    };
    (opt enum $e:ident $( $v:ident $( = $alias:literal )? )+, $raw:expr) => {
        <$e as ::core::str::FromStr>::from_str($raw).map(Some)
    };
    (map $e:ident, $raw:expr) => {
        $crate::parse_level_map::<$e>($raw)
    };
    (opt $b:tt, $raw:expr) => {
        $crate::cfg_parse_single!($b, $raw).map(Some)
    };
    ($b:tt, $raw:expr) => {
        ::core::str::FromStr::from_str($raw.trim())
            .map_err(|_| format!("invalid {} value {:?}", stringify!($b), $raw))
    };
}

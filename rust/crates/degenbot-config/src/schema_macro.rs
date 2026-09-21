//! The `config_schema!` declaration macro.
//!
//! One invocation line per key produces the typed field (Rust type), the
//! `DEGENBOT_*` env mapping, the TOML path, the typed default, and the
//! rendered doc entry. This module is the ONLY place where that expansion
//! logic lives; see `schema::SCHEMA` for the declaration list itself.

/// The declarative schema expansion: generates the typed `BotConfig` tree,
/// section `Default` impls, the path-addressed `assign` setter, and the
/// `SCHEMA` registry — all from ONE invocation line per key.
///
/// A section body is a sequence of key declarations and facet declarations
/// (`name Type { ... }`). A facet generates a typed sub-struct field on its
/// parent and a dotted section path (`strategy.mevblocker_backrun`); a facet body may
/// itself declare keys, which are flattened to `@fk` leaf markers under the
/// facet's dotted section path (`strategy.mevblocker_backrun.bid_mode`) and generate a
/// two-level `assign` arm. `SECTION_PATHS` records every section path so the
/// loader accepts a keyless facet table and resolves each key of a keyed one.
///
/// The expansion first runs [`config_schema_flat!`] to flatten facet keys
/// (a small, self-contained pass over the DECLARATION), then the accumulator
/// muncher emits the tree. Splitting the passes matters: the muncher carries
/// already-generated items/schema entries, so feeding it a nested facet
/// muncher would re-copy those accumulators through extra expansions and
/// blow up superlinearly.
#[macro_export]
macro_rules! config_schema {
    ( $( $sec:ident $Sec:ident { $($body:tt)* } )+ ) => {
        $crate::config_schema_flat! {
            @sections [ $( $sec $Sec { $($body)* } )+ ]
            @out []
        }
    };
}

/// Flatten nested facet key declarations into `@fk <sec> <facet> <key>`
/// leaf markers, leaving the facet shell (with its body) so the emitter can
/// build the typed sub-struct. This pass sees only the raw declaration, so
/// its accumulators stay small.
#[doc(hidden)]
#[macro_export]
macro_rules! config_schema_flat {
    // All sections flattened: hand the flat declaration to the emitter.
    ( @sections [] @out [ $( $out:tt )* ] ) => {
        $crate::config_schema_impl! {
            @sections [ $( $out )* ]
            @bot [] @botdef [] @items [] @schema [] @arms [] @arms_facet [] @paths []
        }
    };

    // Start flattening one section's body.
    (
        @sections [ $sec:ident $Sec:ident { $($body:tt)* } $($rest:tt)* ]
        @out [ $($out:tt)* ]
    ) => {
        $crate::config_schema_flat! {
            @body [$sec] [$Sec] [ $($body)* ] @fout []
            @sections [ $($rest)* ] @out [ $($out)* ]
        }
    };

    // Section body exhausted: emit the flattened section and continue.
    (
        @body [$sec:ident] [$Sec:ident] [] @fout [ $($fout:tt)* ]
        @sections [ $($rest:tt)* ] @out [ $($out:tt)* ]
    ) => {
        $crate::config_schema_flat! {
            @sections [ $($rest)* ]
            @out [ $($out)* $sec $Sec { $($fout)* } ]
        }
    };

    // A normal key declaration passes through unchanged.
    (
        @body [$sec:ident] [$Sec:ident]
        [ $(#[$m:meta])* $f:ident [ $($kt:tt)+ ] = $def:expr, env = $env:literal, def = $def_repr:literal, doc = $doc:literal; $($rest:tt)* ]
        @fout [ $($fout:tt)* ]
        @sections [ $($sr:tt)* ] @out [ $($out:tt)* ]
    ) => {
        $crate::config_schema_flat! {
            @body [$sec] [$Sec] [ $($rest)* ]
            @fout [ $($fout)* $(#[$m])* $f [ $($kt)+ ] = $def, env = $env, def = $def_repr, doc = $doc; ]
            @sections [ $($sr)* ] @out [ $($out)* ]
        }
    };

    // A facet declaration: keep the shell (with its body, so the emitter can
    // build the typed sub-struct) and flatten its inner keys to `@fk` leaves.
    (
        @body [$sec:ident] [$Sec:ident]
        [ $sub:ident $Sub:ident { $($inner:tt)* } $($rest:tt)* ]
        @fout [ $($fout:tt)* ]
        @sections [ $($sr:tt)* ] @out [ $($out:tt)* ]
    ) => {
        $crate::config_schema_flat! {
            @facet [$sec] [$Sec] [$sub] [ $($inner)* ] @fkeys []
            @cont_body [ $($rest)* ]
            @cont_fout [ $($fout)* $sub $Sub { $($inner)* } ]
            @sections [ $($sr)* ] @out [ $($out)* ]
        }
    };

    // Facet inner keys collected: append the leaves and resume the section.
    (
        @facet [$sec:ident] [$Sec:ident] [$sub:ident] [] @fkeys [ $($fkeys:tt)* ]
        @cont_body [ $($rest:tt)* ] @cont_fout [ $($cfout:tt)* ]
        @sections [ $($sr:tt)* ] @out [ $($out:tt)* ]
    ) => {
        $crate::config_schema_flat! {
            @body [$sec] [$Sec] [ $($rest)* ]
            @fout [ $($cfout)* $($fkeys)* ]
            @sections [ $($sr)* ] @out [ $($out)* ]
        }
    };

    // One facet inner key becomes an `@fk` leaf marker.
    (
        @facet [$sec:ident] [$Sec:ident] [$sub:ident]
        [ $(#[$m:meta])* $f:ident [ $($kt:tt)+ ] = $def:expr, env = $env:literal, def = $def_repr:literal, doc = $doc:literal; $($rest:tt)* ]
        @fkeys [ $($fkeys:tt)* ]
        @cont_body [ $($cb:tt)* ] @cont_fout [ $($cf:tt)* ]
        @sections [ $($sr:tt)* ] @out [ $($out:tt)* ]
    ) => {
        $crate::config_schema_flat! {
            @facet [$sec] [$Sec] [$sub] [ $($rest)* ]
            @fkeys [
                $($fkeys)*
                @fk $sec $sub $(#[$m])* $f [ $($kt)+ ] = $def, env = $env, def = $def_repr, doc = $doc;
            ]
            @cont_body [ $($cb)* ] @cont_fout [ $($cf)* ]
            @sections [ $($sr)* ] @out [ $($out)* ]
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! config_schema_impl {
    // No sections left: emit the whole tree.
    (
        @sections []
        @bot [$($bot:tt)*]
        @botdef [$($botdef:tt)*]
        @items [$($items:tt)*]
        @schema [$($schema:tt)*]
        @arms [ $([$as:ident, $af:ident, $($akt:tt)+],)* ]
        @arms_facet [ $([$fs:ident, $fsu:ident, $ff:ident, $($fkt:tt)+],)* ]
        @paths [$($paths:tt)*]
    ) => {
        /// The typed configuration tree. Every field is generated from the
        /// same declaration that produced `SCHEMA` — one site per key
        /// (see the crate docs for the parity + precedence contract).
        #[derive(Debug, Clone, PartialEq)]
        pub struct BotConfig { $($bot)* }

        $($items)*

        impl ::core::default::Default for BotConfig {
            fn default() -> Self {
                Self { $($botdef)* }
            }
        }

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
                        (stringify!($as), stringify!($af)) => {
                            self.$as.$af = $crate::cfg_parse_single!($($akt)+, raw)
                                .map_err(|e| {
                                    format!(
                                        "{}: {e}",
                                        concat!(stringify!($as), ".", stringify!($af))
                                    )
                                })?;
                            Ok(())
                        }
                    )*
                    $(
                        (concat!(stringify!($fs), ".", stringify!($fsu)), stringify!($ff)) => {
                            self.$fs.$fsu.$ff = $crate::cfg_parse_single!($($fkt)+, raw)
                                .map_err(|e| {
                                    format!(
                                        "{}: {e}",
                                        concat!(
                                            stringify!($fs), ".",
                                            stringify!($fsu), ".",
                                            stringify!($ff)
                                        )
                                    )
                                })?;
                            Ok(())
                        }
                    )*
                    _ => Ok(()),
                }
            }
        }

        /// The self-describing key registry: one entry per declared key, in
        /// declaration order (which fixes doc + loader iteration order).
        pub const SCHEMA: &[$crate::schema::KeyDecl] = &[ $($schema)* ];

        /// Every declared section path, including empty facet namespaces that
        /// declare no keys (dotted for nested facets). The loader consults
        /// this so a keyless facet table is valid rather than "unknown" and a
        /// keyed one resolves its leaves.
        pub const SECTION_PATHS: &[&str] = &[ $($paths)* ];
    };

    // Start scanning one section's body.
    (
        @sections [ $sec:ident $Sec:ident { $($body:tt)* } $($rest:tt)* ]
        @bot [$($bot:tt)*]
        @botdef [$($botdef:tt)*]
        @items [$($items:tt)*]
        @schema [$($schema:tt)*]
        @arms [$($arms:tt)*]
        @arms_facet [$($arms_facet:tt)*]
        @paths [$($paths:tt)*]
    ) => {
        $crate::config_schema_impl! {
            @body [$sec] [$Sec] [ $($body)* ]
            @fields [] @defaults [] @aux []
            @key_schema [] @key_arms [] @key_facet_arms [] @key_paths []
            @rest [ $($rest)* ]
            @bot [$($bot)*] @botdef [$($botdef)*] @items [$($items)*]
            @schema_out [$($schema)*] @arms_out [$($arms)*]
            @arms_facet_out [$($arms_facet)*] @paths_out [$($paths)*]
        }
    };

    // A flattened facet leaf (`@fk <sec> <facet> <key>`): the key lands under
    // the facet's dotted section path and gets a two-level `assign` arm.
    (
        @body [$sec:ident] [$Sec:ident]
        [ @fk $fsec:ident $fsub:ident $(#[$fmeta:meta])* $f:ident [ $($kt:tt)+ ] = $def:expr, env = $env:literal, def = $def_repr:literal, doc = $doc:literal; $($rest:tt)* ]
        @fields [$($fields:tt)*]
        @defaults [$($defaults:tt)*]
        @aux [$($aux:tt)*]
        @key_schema [$($key_schema:tt)*]
        @key_arms [$($key_arms:tt)*]
        @key_facet_arms [$($key_facet_arms:tt)*]
        @key_paths [$($key_paths:tt)*]
        @rest [$($body_rest:tt)*]
        @bot [$($bot:tt)*]
        @botdef [$($botdef:tt)*]
        @items [$($items:tt)*]
        @schema_out [$($schema_out:tt)*]
        @arms_out [$($arms_out:tt)*]
        @arms_facet_out [$($arms_facet_out:tt)*]
        @paths_out [$($paths_out:tt)*]
    ) => {
        $crate::config_schema_impl! {
            @body [$sec] [$Sec] [ $($rest)* ]
            @fields [$($fields)*]
            @defaults [$($defaults)*]
            @aux [$($aux)*]
            @key_schema [
                $($key_schema)*
                $crate::schema::KeyDecl {
                    section: concat!(stringify!($fsec), ".", stringify!($fsub)),
                    field: stringify!($f),
                    env: $env,
                    toml_path: concat!(
                        stringify!($fsec), ".", stringify!($fsub), ".", stringify!($f)
                    ),
                    kind: $crate::cfg_kind!($($kt)+),
                    default_repr: $def_repr,
                    description: $doc,
                },
            ]
            @key_arms [$($key_arms)*]
            @key_facet_arms [ $($key_facet_arms)* [$fsec, $fsub, $f, $($kt)+], ]
            @key_paths [$($key_paths)*]
            @rest [$($body_rest)*]
            @bot [$($bot)*] @botdef [$($botdef)*] @items [$($items)*]
            @schema_out [$($schema_out)*] @arms_out [$($arms_out)*]
            @arms_facet_out [$($arms_facet_out)*] @paths_out [$($paths_out)*]
        }
    };

    // A key declaration inside a section body.
    (
        @body [$sec:ident] [$Sec:ident]
        [ $(#[$fmeta:meta])* $f:ident [ $($kt:tt)+ ] = $def:expr, env = $env:literal, def = $def_repr:literal, doc = $doc:literal; $($rest:tt)* ]
        @fields [$($fields:tt)*]
        @defaults [$($defaults:tt)*]
        @aux [$($aux:tt)*]
        @key_schema [$($key_schema:tt)*]
        @key_arms [$($key_arms:tt)*]
        @key_facet_arms [$($key_facet_arms:tt)*]
        @key_paths [$($key_paths:tt)*]
        @rest [$($body_rest:tt)*]
        @bot [$($bot:tt)*]
        @botdef [$($botdef:tt)*]
        @items [$($items:tt)*]
        @schema_out [$($schema_out:tt)*]
        @arms_out [$($arms_out:tt)*]
        @arms_facet_out [$($arms_facet_out:tt)*]
        @paths_out [$($paths_out:tt)*]
    ) => {
        $crate::config_schema_impl! {
            @body [$sec] [$Sec] [ $($rest)* ]
            @fields [$($fields)* pub $f: $crate::cfg_ty!($($kt)+), ]
            @defaults [$($defaults)* $f: $def, ]
            @aux [$($aux)* $crate::cfg_enum!($($kt)+); ]
            @key_schema [
                $($key_schema)*
                $crate::schema::KeyDecl {
                    section: stringify!($sec),
                    field: stringify!($f),
                    env: $env,
                    toml_path: concat!(stringify!($sec), ".", stringify!($f)),
                    kind: $crate::cfg_kind!($($kt)+),
                    default_repr: $def_repr,
                    description: $doc,
                },
            ]
            @key_arms [ $($key_arms)* [$sec, $f, $($kt)+], ]
            @key_facet_arms [$($key_facet_arms)*]
            @key_paths [$($key_paths)*]
            @rest [$($body_rest)*]
            @bot [$($bot)*] @botdef [$($botdef)*] @items [$($items)*]
            @schema_out [$($schema_out)*] @arms_out [$($arms_out)*]
            @arms_facet_out [$($arms_facet_out)*] @paths_out [$($paths_out)*]
        }
    };

    // A facet declaration inside a section body: emit its typed sub-struct
    // (fields built by the self-contained `config_facet_emit!`) and record
    // its dotted section path.
    (
        @body [$sec:ident] [$Sec:ident]
        [ $sub:ident $Sub:ident { $($inner:tt)* } $($rest:tt)* ]
        @fields [$($fields:tt)*]
        @defaults [$($defaults:tt)*]
        @aux [$($aux:tt)*]
        @key_schema [$($key_schema:tt)*]
        @key_arms [$($key_arms:tt)*]
        @key_facet_arms [$($key_facet_arms:tt)*]
        @key_paths [$($key_paths:tt)*]
        @rest [$($body_rest:tt)*]
        @bot [$($bot:tt)*]
        @botdef [$($botdef:tt)*]
        @items [$($items:tt)*]
        @schema_out [$($schema_out:tt)*]
        @arms_out [$($arms_out:tt)*]
        @arms_facet_out [$($arms_facet_out:tt)*]
        @paths_out [$($paths_out:tt)*]
    ) => {
        $crate::config_schema_impl! {
            @body [$sec] [$Sec] [ $($rest)* ]
            @fields [$($fields)* pub $sub: $Sub, ]
            @defaults [$($defaults)* $sub: $Sub::default(), ]
            @aux [$($aux)* $crate::config_facet_emit!($Sub { $($inner)* }); ]
            @key_schema [$($key_schema)*]
            @key_arms [$($key_arms)*]
            @key_facet_arms [$($key_facet_arms)*]
            @key_paths [ $($key_paths)* concat!(stringify!($sec), ".", stringify!($sub)), ]
            @rest [$($body_rest)*]
            @bot [$($bot)*] @botdef [$($botdef)*] @items [$($items)*]
            @schema_out [$($schema_out)*] @arms_out [$($arms_out)*]
            @arms_facet_out [$($arms_facet_out)*] @paths_out [$($paths_out)*]
        }
    };

    // Section body exhausted: emit the section struct + Default into @items and
    // continue with the next section.
    (
        @body [$sec:ident] [$Sec:ident] []
        @fields [$($fields:tt)*]
        @defaults [$($defaults:tt)*]
        @aux [$($aux:tt)*]
        @key_schema [$($key_schema:tt)*]
        @key_arms [$($key_arms:tt)*]
        @key_facet_arms [$($key_facet_arms:tt)*]
        @key_paths [$($key_paths:tt)*]
        @rest [$($body_rest:tt)*]
        @bot [$($bot:tt)*]
        @botdef [$($botdef:tt)*]
        @items [$($items:tt)*]
        @schema_out [$($schema_out:tt)*]
        @arms_out [$($arms_out:tt)*]
        @arms_facet_out [$($arms_facet_out:tt)*]
        @paths_out [$($paths_out:tt)*]
    ) => {
        $crate::config_schema_impl! {
            @sections [ $($body_rest)* ]
            @bot [$($bot)* pub $sec: $Sec, ]
            @botdef [$($botdef)* $sec: $Sec::default(), ]
            @items [
                $($items)*
                $($aux)*
                #[doc = concat!("Configuration section `", stringify!($sec), "`.")]
                #[derive(Debug, Clone, PartialEq)]
                pub struct $Sec { $($fields)* }

                impl ::core::default::Default for $Sec {
                    fn default() -> Self {
                        Self { $($defaults)* }
                    }
                }
            ]
            @schema [$($schema_out)* $($key_schema)* ]
            @arms [$($arms_out)* $($key_arms)* ]
            @arms_facet [$($arms_facet_out)* $($key_facet_arms)* ]
            @paths [$($paths_out)* stringify!($sec), $($key_paths)* ]
        }
    };
}

/// Emit one facet's typed sub-struct and `Default` from its key body. A
/// self-contained muncher (its accumulators hold only the facet's own
/// fields) so it can be expanded from inside the emitter without dragging
/// the parent's generated accumulators along.
#[doc(hidden)]
#[macro_export]
macro_rules! config_facet_emit {
    ( $Sub:ident { $($inner:tt)* } ) => {
        $crate::config_facet_emit_impl! {
            $Sub { $($inner)* } @fields [] @defaults []
        }
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! config_facet_emit_impl {
    (
        $Sub:ident { }
        @fields [ $($fields:tt)* ]
        @defaults [ $($defaults:tt)* ]
    ) => {
        #[doc = concat!("Strategy-facet configuration section `", stringify!($Sub), "`.")]
        #[derive(Debug, Clone, PartialEq)]
        pub struct $Sub { $($fields)* }

        impl ::core::default::Default for $Sub {
            fn default() -> Self {
                Self { $($defaults)* }
            }
        }
    };

    (
        $Sub:ident { $(#[$m:meta])* $f:ident [ $($kt:tt)+ ] = $def:expr, env = $env:literal, def = $def_repr:literal, doc = $doc:literal; $($rest:tt)* }
        @fields [ $($fields:tt)* ]
        @defaults [ $($defaults:tt)* ]
    ) => {
        $crate::config_facet_emit_impl! {
            $Sub { $($rest)* }
            @fields [ $($fields)* pub $f: $crate::cfg_ty!($($kt)+), ]
            @defaults [ $($defaults)* $f: $def, ]
        }
    };
}

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

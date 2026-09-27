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
/// (`name Type { ... }`). A key names its env layer either `env = "NAME"` (one
/// name for the key) or `env_prefix = "PREFIX_"` (one name per entry, for a
/// table whose keys the operator picks at runtime). A facet generates a typed
/// sub-struct field on its parent and a dotted section path
/// (`strategy.mevblocker_backrun`); a facet body may
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

    // A family-shaped key declaration (`env_prefix` in place of one env
    // name) passes through with its prefix intact: the flat pass only
    // restructures nesting, it does not decide what the env layer is.
    (
        @body [$sec:ident] [$Sec:ident]
        [ $(#[$m:meta])* $f:ident [ $($kt:tt)+ ] = $def:expr, env_prefix = $env:literal, def = $def_repr:literal, doc = $doc:literal; $($rest:tt)* ]
        @fout [ $($fout:tt)* ]
        @sections [ $($sr:tt)* ] @out [ $($out:tt)* ]
    ) => {
        $crate::config_schema_flat! {
            @body [$sec] [$Sec] [ $($rest)* ]
            @fout [ $($fout)* $(#[$m])* $f [ $($kt)+ ] = $def, env_prefix = $env, def = $def_repr, doc = $doc; ]
            @sections [ $($sr)* ] @out [ $($out)* ]
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

    // A family-shaped facet inner key becomes an `@fk` leaf marker too; the
    // facet body is opaque to the flattening pass, so the marker carries the
    // prefix form and the emitter splits it.
    (
        @facet [$sec:ident] [$Sec:ident] [$sub:ident]
        [ $(#[$m:meta])* $f:ident [ $($kt:tt)+ ] = $def:expr, env_prefix = $env:literal, def = $def_repr:literal, doc = $doc:literal; $($rest:tt)* ]
        @fkeys [ $($fkeys:tt)* ]
        @cont_body [ $($cb:tt)* ] @cont_fout [ $($cf:tt)* ]
        @sections [ $($sr:tt)* ] @out [ $($out:tt)* ]
    ) => {
        $crate::config_schema_flat! {
            @facet [$sec] [$Sec] [$sub] [ $($rest)* ]
            @fkeys [
                $($fkeys)*
                @fk $sec $sub $(#[$m])* $f [ $($kt)+ ] = $def, env_prefix = $env, def = $def_repr, doc = $doc;
            ]
            @cont_body [ $($cb)* ] @cont_fout [ $($cf)* ]
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

        impl BotConfig {
            /// Read ONE declared key's typed value, addressed by its section
            /// and field. Generated from the same arms as [`Self::assign`], so
            /// a key becomes readable the moment it is declared: a surface
            /// that walks the schema reads every key without a hand-written
            /// accessor beside it.
            ///
            /// Returns `None` for an undeclared key AND for a declared
            /// unset-able key the operator left alone. The two absences are
            /// deliberately the same answer, because neither has a value to
            /// report, and reporting the declared default instead would let a
            /// consumer read "the operator said nothing" as "the operator
            /// chose this".
            #[must_use]
            pub fn value(
                &self,
                section: &str,
                field: &str,
            ) -> Option<$crate::schema::ConfigValue<'_>> {
                match (section, field) {
                    $(
                        (stringify!($as), stringify!($af)) => {
                            $crate::cfg_readable!([$($akt)+] &self.$as.$af)
                        }
                    )*
                    $(
                        (concat!(stringify!($fs), ".", stringify!($fsu)), stringify!($ff)) => {
                            $crate::cfg_readable!([$($fkt)+] &self.$fs.$fsu.$ff)
                        }
                    )*
                    _ => None,
                }
            }
        }

        /// Every `(section, field)` the generated [`BotConfig::value`] reader
        /// answers, so a census can compare the reader against `SCHEMA`
        /// instead of trusting the expansion to have covered every arm.
        pub const READABLE_KEYS: &[(&str, &str)] = &[
            $( (stringify!($as), stringify!($af)), )*
            $( (concat!(stringify!($fs), ".", stringify!($fsu)), stringify!($ff)), )*
        ];

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

    // A family-shaped flattened facet leaf: the prefix is the entry's env
    // layer AND the one name labels and the shadow check print, so the
    // registry carries it in both slots.
    (
        @body [$sec:ident] [$Sec:ident]
        [ @fk $fsec:ident $fsub:ident $(#[$fmeta:meta])* $f:ident [ $($kt:tt)+ ] = $def:expr, env_prefix = $env:literal, def = $def_repr:literal, doc = $doc:literal; $($rest:tt)* ]
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
                    env_prefix: ::core::option::Option::Some($env),
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
                    env_prefix: ::core::option::Option::None,
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

    // A family-shaped key declaration inside a section body: the prefix
    // stands in for the env name and is recorded as the family too.
    (
        @body [$sec:ident] [$Sec:ident]
        [ $(#[$fmeta:meta])* $f:ident [ $($kt:tt)+ ] = $def:expr, env_prefix = $env:literal, def = $def_repr:literal, doc = $doc:literal; $($rest:tt)* ]
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
                    env_prefix: ::core::option::Option::Some($env),
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
                    env_prefix: ::core::option::Option::None,
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

    // A family-shaped facet key: the sub-struct needs only the field, type
    // and default, so the env form is matched and dropped here.
    (
        $Sub:ident { $(#[$m:meta])* $f:ident [ $($kt:tt)+ ] = $def:expr, env_prefix = $env:literal, def = $def_repr:literal, doc = $doc:literal; $($rest:tt)* }
        @fields [ $($fields:tt)* ]
        @defaults [ $($defaults:tt)* ]
    ) => {
        $crate::config_facet_emit_impl! {
            $Sub { $($rest)* }
            @fields [ $($fields)* pub $f: $crate::cfg_ty!($($kt)+), ]
            @defaults [ $($defaults)* $f: $def, ]
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

/// Read one declared field as a [`$crate::schema::ConfigValue`], preserving the
/// kind the declaration named.
#[doc(hidden)]
#[macro_export]
macro_rules! cfg_value {
    (bool, $v:expr) => {
        $crate::schema::ConfigValue::Bool(*$v)
    };
    (bool_not, $v:expr) => {
        $crate::schema::ConfigValue::Bool(*$v)
    };
    (ms, $v:expr) => {
        $crate::schema::ConfigValue::Uint(u128::from(*$v))
    };
    (string, $v:expr) => {
        $crate::schema::ConfigValue::Text(::std::borrow::Cow::Borrowed($v.as_str()))
    };
    (path, $v:expr) => {
        $crate::schema::ConfigValue::Path(::std::borrow::Cow::Borrowed($v.as_path()))
    };
    (usize, $v:expr) => {
        $crate::schema::ConfigValue::Uint(u128::from(*$v as u64))
    };
    (u64, $v:expr) => {
        $crate::schema::ConfigValue::Uint(u128::from(*$v))
    };
    (u128, $v:expr) => {
        $crate::schema::ConfigValue::Uint(*$v)
    };
    (i64, $v:expr) => {
        $crate::schema::ConfigValue::Int(*$v)
    };
    (i32, $v:expr) => {
        $crate::schema::ConfigValue::Int(i64::from(*$v))
    };
    (f64, $v:expr) => {
        $crate::schema::ConfigValue::Float(*$v)
    };
    (enum $e:ident $( $v:ident $( = $alias:literal )? )+, $val:expr) => {
        $crate::schema::ConfigValue::Enum(::std::borrow::Cow::Owned($val.to_string()))
    };
    (map $e:ident, $v:expr) => {
        $crate::schema::ConfigValue::Map(
            $v.iter()
                .map(|(key, value)| (key.clone(), value.to_string()))
                .collect(),
        )
    };
    (strmap, $v:expr) => {
        $crate::schema::ConfigValue::Map(
            $v.iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        )
    };
}

/// Read one declared field as an `Option` of the value its kind names: a
/// required key always has one, an unset-able key has one only when the
/// operator supplied it. The kind tokens arrive bracketed so the repetition
/// cannot swallow the separator (a bare `tt` repetition ahead of a comma is a
/// local-ambiguity error).
#[doc(hidden)]
#[macro_export]
macro_rules! cfg_readable {
    ([opt enum $e:ident $( $v:ident $( = $alias:literal )? )+] $val:expr) => {
        $val.as_ref().map(|inner| $crate::cfg_value!(enum $e $( $v $( = $alias )? )+, inner))
    };
    ([opt $inner:tt] $val:expr) => {
        $val.as_ref().map(|inner| $crate::cfg_value!($inner, inner))
    };
    ([$($kind:tt)+] $val:expr) => {
        ::core::option::Option::Some($crate::cfg_value!($($kind)+, $val))
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
    (strmap) => { ::std::collections::BTreeMap<::std::string::String, ::std::string::String> };
    (opt strmap) => { Option<::std::collections::BTreeMap<::std::string::String, ::std::string::String>> };
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
    (opt strmap) => {
        $crate::schema::ValueKind { base: $crate::schema::BaseKind::StrMap, optional: true }
    };
    (strmap) => {
        $crate::schema::ValueKind { base: $crate::schema::BaseKind::StrMap, optional: false }
    };
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
    (strmap, $raw:expr) => {
        $crate::parse_string_map($raw)
    };
    (opt $b:tt, $raw:expr) => {
        $crate::cfg_parse_single!($b, $raw).map(Some)
    };
    ($b:tt, $raw:expr) => {
        ::core::str::FromStr::from_str($raw.trim())
            .map_err(|_| format!("invalid {} value {:?}", stringify!($b), $raw))
    };
}

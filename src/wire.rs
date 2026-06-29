//! Shared machinery for *wire enums* — small C-like enums that cross a
//! persistence/pipe boundary as a fixed string vocabulary. No zellij-tile
//! dependency.
//!
//! The point is to make the round-trip a property of *construction* rather than
//! of a hand-written pair of `match` blocks plus a hand-written serde pair: a
//! type's `as_wire`/`from_wire` and its `Serialize`/`Deserialize` are all
//! generated from one table, so they cannot drift. There are two unknown-token
//! policies, picked per type:
//!
//! - **lenient** — `from_wire(&str) -> Self`; unknown/absent tokens fall back to
//!   a designated variant, and deserialization never fails. Used by [`Status`],
//!   whose `statuses!` table calls [`wire_serde!`] directly (it owns a richer
//!   table that also carries role + glyph data, so it can't be a plain
//!   [`wire_enum!`]).
//! - **strict** — `from_wire(&str) -> Option<Self>`; unknown tokens deserialize
//!   to an *error* so a corrupt snapshot entry can't masquerade as valid. Used
//!   by [`ObservationOrigin`] via [`wire_enum!`].
//!
//! [`Status`]: crate::status::Status
//! [`ObservationOrigin`]: crate::observation::ObservationOrigin

/// Generate `serde::Serialize` + `serde::Deserialize` for a type that already
/// has `as_wire(self) -> &'static str` and a `from_wire` matching the policy.
/// Serialization is identical for both policies (`serialize_str(as_wire())`);
/// only deserialization differs in how it treats an unknown token.
macro_rules! wire_serde {
    // `from_wire(&str) -> Self` — unknown tokens already fold into a fallback.
    (lenient, $T:ty) => {
        impl serde::Serialize for $T {
            fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
                ser.serialize_str(self.as_wire())
            }
        }
        impl<'de> serde::Deserialize<'de> for $T {
            fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
                Ok(<$T>::from_wire(&<String as serde::Deserialize>::deserialize(de)?))
            }
        }
    };
    // `from_wire(&str) -> Option<Self>` — an unknown token is a hard error.
    (strict, $T:ty) => {
        impl serde::Serialize for $T {
            fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
                ser.serialize_str(self.as_wire())
            }
        }
        impl<'de> serde::Deserialize<'de> for $T {
            fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
                let raw = <String as serde::Deserialize>::deserialize(de)?;
                <$T>::from_wire(&raw).ok_or_else(|| {
                    serde::de::Error::custom(format!(
                        "unknown {} wire token: {raw:?}",
                        stringify!($T),
                    ))
                })
            }
        }
    };
}
pub(crate) use wire_serde;

/// Define a strict wire enum and everything that varies per variant from one
/// table: the enum, `ALL`, `as_wire`, `from_wire` (`-> Option<Self>`), and the
/// serde pair (via [`wire_serde!`]). Each row is `Variant => "wire"`.
///
/// The variant list and the wire vocabulary become a *single source of truth* —
/// `as_wire` is an exhaustive `match self` (a dropped row fails to compile) and
/// `from_wire` is its literal inverse, so the round-trip holds by construction.
/// Pass the enum's derives/docs as ordinary attributes; an unknown token
/// deserializes to an error (the strict policy — see the module docs).
///
/// `Status` is intentionally *not* built with this: it owns a richer
/// `statuses!` table (wire + role + glyph in one place, lenient policy) and
/// calls [`wire_serde!`] directly, so its wire and presentation vocabularies
/// stay in one table rather than being split across two.
macro_rules! wire_enum {
    (
        $(#[$enum_meta:meta])*
        $vis:vis enum $Name:ident {
            $( $(#[$vmeta:meta])* $variant:ident => $wire:literal ),+ $(,)?
        }
    ) => {
        $(#[$enum_meta])*
        $vis enum $Name {
            $( $(#[$vmeta])* $variant ),+
        }

        impl $Name {
            /// Every variant, in table order. Lets callers and exhaustiveness
            /// tests iterate without re-typing the list. The macro emits this for
            /// every enum uniformly; some (e.g. `Status`) use it in production
            /// while others are test-only, so allow it to go unused.
            #[allow(dead_code)]
            pub const ALL: &'static [$Name] = &[ $( $Name::$variant ),+ ];

            /// The wire token for this variant.
            pub fn as_wire(self) -> &'static str {
                match self {
                    $( $Name::$variant => $wire, )+
                }
            }

            /// Parse a wire token; an unknown token yields `None` so the caller
            /// drops the entry rather than guessing.
            pub fn from_wire(raw: &str) -> Option<$Name> {
                match raw {
                    $( $wire => Some($Name::$variant), )+
                    _ => None,
                }
            }
        }

        $crate::wire::wire_serde!(strict, $Name);
    };
}
pub(crate) use wire_enum;

#[cfg(test)]
mod tests {
    // Cover the model natively — independent of `Status` / `ObservationOrigin`,
    // so the macros stay pinned even if both production types change. Throwaway
    // enums exercise each policy end-to-end.

    // ── strict policy, built end-to-end by `wire_enum!` ──────────────────────
    wire_enum! {
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        enum Color {
            Red => "red",
            Green => "grn",
        }
    }

    #[test]
    fn wire_enum_generates_all_as_wire_and_inverse_from_wire() {
        assert_eq!(Color::ALL, &[Color::Red, Color::Green]);
        for &c in Color::ALL {
            assert_eq!(Color::from_wire(c.as_wire()), Some(c)); // round-trip by construction
        }
        assert_eq!(Color::Green.as_wire(), "grn");
        assert_eq!(Color::from_wire("nope"), None);
    }

    #[test]
    fn strict_serializes_as_wire_token_and_round_trips_through_json() {
        assert_eq!(serde_json::to_string(&Color::Green).unwrap(), r#""grn""#);
        for &c in Color::ALL {
            let json = serde_json::to_string(&c).unwrap();
            assert_eq!(serde_json::from_str::<Color>(&json).unwrap(), c);
        }
    }

    #[test]
    fn strict_deserialize_rejects_unknown_token_naming_the_type() {
        let err = serde_json::from_str::<Color>(r#""violet""#).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Color"), "error names the type: {msg}");
        assert!(msg.contains("violet"), "error quotes the bad token: {msg}");
    }

    // ── lenient policy, applied by `wire_serde!` to a hand-written enum ───────
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Mood {
        Calm,
        Loud,
    }

    impl Mood {
        fn as_wire(self) -> &'static str {
            match self {
                Mood::Calm => "calm",
                Mood::Loud => "loud",
            }
        }

        // Lenient: anything unknown/absent folds into the `Calm` fallback.
        fn from_wire(s: &str) -> Mood {
            match s {
                "loud" => Mood::Loud,
                _ => Mood::Calm,
            }
        }
    }

    wire_serde!(lenient, Mood);

    #[test]
    fn lenient_round_trips_known_and_folds_unknown_into_fallback() {
        assert_eq!(serde_json::to_string(&Mood::Loud).unwrap(), r#""loud""#);
        assert_eq!(serde_json::from_str::<Mood>(r#""loud""#).unwrap(), Mood::Loud);
        // Unknown token deserializes to the fallback instead of erroring.
        assert_eq!(serde_json::from_str::<Mood>(r#""???""#).unwrap(), Mood::Calm);
    }
}

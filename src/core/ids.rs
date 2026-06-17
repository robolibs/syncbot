//! Strong-typed IDs. Mirrors `include/syncbot/ids.hpp` — each id is a u64
//! tagged by name, with `From<u64>` and `raw()` for round-tripping.

use serde::{Deserialize, Serialize};

macro_rules! id_newtype {
    ($name:ident) => {
        #[derive(
            Debug,
            Clone,
            Copy,
            PartialEq,
            Eq,
            Hash,
            Default,
            PartialOrd,
            Ord,
            Serialize,
            Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl $name {
            pub const fn new(v: u64) -> Self {
                Self(v)
            }
            pub const fn raw(self) -> u64 {
                self.0
            }
        }

        impl From<u64> for $name {
            fn from(v: u64) -> Self {
                Self(v)
            }
        }

        impl From<$name> for u64 {
            fn from(v: $name) -> Self {
                v.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

id_newtype!(RobotId);
id_newtype!(MissionId);
id_newtype!(ClaimId);
id_newtype!(LeaseId);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let r = RobotId::from(42);
        assert_eq!(r.raw(), 42);
        assert_eq!(u64::from(r), 42);
        assert_eq!(format!("{}", r), "42");
    }

    #[test]
    fn distinct_types_share_repr() {
        let a = ClaimId::new(7);
        let b = LeaseId::new(7);
        assert_eq!(a.raw(), b.raw());
        // The compiler enforces these don't unify — that's the point.
    }
}

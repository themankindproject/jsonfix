//! Repair switches and partial-parsing policy.

use core::fmt;
use core::ops::BitOr;

/// Which incomplete constructs a partial parse is allowed to keep.
///
/// Flags mirror the `Allow` bitmask of the widely used `partial-json`
/// libraries: with `Allow::OBJ | Allow::STR`, the input `{"key": "v` parses to
/// `{"key": "v"}`, while the input `{"key": "v, "x` keeps `"v"` and drops the
/// unfinished member.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Allow(u16);

impl Allow {
    /// Keep nothing incomplete; every value must be complete.
    pub const NOTHING: Allow = Allow(0);
    /// Keep a string that is cut off before its closing quote.
    pub const STR: Allow = Allow(1 << 0);
    /// Keep a number that is cut off (`1.`, `2e`, `-`).
    pub const NUM: Allow = Allow(1 << 1);
    /// Keep an array that is cut off before its closing bracket.
    pub const ARR: Allow = Allow(1 << 2);
    /// Keep an object that is cut off before its closing brace.
    pub const OBJ: Allow = Allow(1 << 3);
    /// Keep an object whose key is cut off.
    pub const KEY: Allow = Allow(1 << 4);
    /// Keep a boolean that is cut off (`tru`, `fal`).
    pub const BOOL: Allow = Allow(1 << 5);
    /// Keep a `null` that is cut off (`nul`).
    pub const NULL: Allow = Allow(1 << 6);
    /// Both scalar keywords: [`Allow::BOOL`] plus [`Allow::NULL`].
    pub const ATOM: Allow = Allow(Allow::BOOL.0 | Allow::NULL.0);
    /// Both collections: [`Allow::ARR`] plus [`Allow::OBJ`].
    pub const COLLECTION: Allow = Allow(Allow::ARR.0 | Allow::OBJ.0);
    /// Every flag; the default for [`Options::all`].
    pub const ALL: Allow = Allow(0b0111_1111);
    /// Alias for [`Allow::NOTHING`], for symmetry with [`Repairs::NONE`].
    pub const NONE: Allow = Allow::NOTHING;

    /// Whether every flag in `other` is present in `self`.
    pub const fn contains(self, other: Allow) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Whether no flag is set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The raw bitmask.
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Set union.
    pub const fn union(self, other: Allow) -> Allow {
        Allow(self.0 | other.0)
    }

    /// Set difference.
    pub const fn without(self, other: Allow) -> Allow {
        Allow(self.0 & !other.0)
    }
}

impl BitOr for Allow {
    type Output = Allow;

    fn bitor(self, rhs: Allow) -> Allow {
        self.union(rhs)
    }
}

impl fmt::Debug for Allow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Allow(0b{:07b})", self.0)
    }
}

/// Which repair passes run. Construct one with `|` or start from a preset.
///
/// Each flag is independent, so a caller can accept markdown fences and
/// comments while refusing to invent quoting for bare words:
///
/// ```
/// use jsonfix::{repair_with, Options, Repairs};
///
/// let opts = Options::all().with_repairs(Repairs::FENCES | Repairs::COMMENTS);
/// assert_eq!(repair_with("```json\n// hi\n{\"a\": 1}\n```", opts).unwrap(), "{\"a\": 1}");
/// ```
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Repairs(u32);

impl Repairs {
    /// Run no repair pass: the input must already be valid JSON.
    pub const NONE: Repairs = Repairs(0);
    /// Strip markdown code fences such as ```` ```json ````.
    pub const FENCES: Repairs = Repairs(1 << 0);
    /// Strip `//` line comments and `/* ... */` block comments.
    pub const COMMENTS: Repairs = Repairs(1 << 1);
    /// Quote bare object keys and bare string values.
    pub const UNQUOTED: Repairs = Repairs(1 << 2);
    /// Convert `True`/`False`/`None`, `undefined`, and lone `NaN`/`Infinity`.
    pub const KEYWORDS: Repairs = Repairs(1 << 3);
    /// Fix numeric literals: leading zeros, `.5`, `2.`, `2e`, a lone `-`.
    pub const NUMBERS: Repairs = Repairs(1 << 4);
    /// Join adjacent strings across `+`, as produced by code generators.
    pub const CONCATENATION: Repairs = Repairs(1 << 5);
    /// Unwrap `NumberLong(2)`, `ISODate("...")`, and JSONP `cb({...})` wrappers.
    pub const CALLS: Repairs = Repairs(1 << 6);
    /// Decode HTML entities such as `&quot;` and `&#34;`.
    pub const ENTITIES: Repairs = Repairs(1 << 7);
    /// Accept `'single quoted'` strings and typographic quotes (`“...”`).
    pub const QUOTES: Repairs = Repairs(1 << 8);
    /// Accept non-ASCII whitespace such as U+00A0 and U+3000.
    pub const WHITESPACE: Repairs = Repairs(1 << 9);
    /// Tolerate input that ends early: close open brackets and strings.
    pub const TRUNCATION: Repairs = Repairs(1 << 10);
    /// Turn several top-level values (newline/comma separated) into an array.
    pub const NDJSON: Repairs = Repairs(1 << 11);
    /// Every pass: the default.
    pub const ALL: Repairs = Repairs(0b1111_1111_1111);

    /// Whether every flag in `other` is present in `self`.
    pub const fn contains(self, other: Repairs) -> bool {
        (self.0 & other.0) == other.0
    }

    /// The raw bitmask.
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Set union.
    pub const fn union(self, other: Repairs) -> Repairs {
        Repairs(self.0 | other.0)
    }

    /// Set difference.
    pub const fn without(self, other: Repairs) -> Repairs {
        Repairs(self.0 & !other.0)
    }
}

impl BitOr for Repairs {
    type Output = Repairs;

    fn bitor(self, rhs: Repairs) -> Repairs {
        self.union(rhs)
    }
}

impl fmt::Debug for Repairs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Repairs(0b{:011b})", self.0)
    }
}

/// How [`repair_with`](crate::repair_with) and friends behave.
///
/// ```
/// use jsonfix::{parse_partial, Allow, Options};
///
/// let opts = Options::partial(Allow::OBJ | Allow::STR);
/// assert_eq!(parse_partial("{\"a\": \"v", opts).unwrap().to_json_string(), "{\"a\": \"v\"}");
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Options {
    /// Which complete values are kept out of values cut off at end of input.
    pub allow: Allow,
    /// Which repair passes run.
    pub repairs: Repairs,
}

impl Options {
    /// Repair everything, keep every incomplete trailing value.
    pub const ALL: Options = Options {
        allow: Allow::ALL,
        repairs: Repairs::ALL,
    };

    /// Repair everything, keep every incomplete trailing value.
    pub const fn all() -> Self {
        Self::ALL
    }

    /// No repairs, nothing incomplete: the input must be valid JSON.
    pub const fn strict() -> Self {
        Self {
            allow: Allow::NOTHING,
            repairs: Repairs::NONE,
        }
    }

    /// Full repair with an explicit partial-parsing policy.
    pub const fn partial(allow: Allow) -> Self {
        Self {
            allow,
            repairs: Repairs::ALL,
        }
    }

    /// Replaces the repair mask.
    pub const fn with_repairs(mut self, repairs: Repairs) -> Self {
        self.repairs = repairs;
        self
    }

    /// Replaces the partial-parsing policy.
    pub const fn with_allow(mut self, allow: Allow) -> Self {
        self.allow = allow;
        self
    }

    /// Whether the `flag` repair pass is enabled.
    pub const fn repairs(self, flag: Repairs) -> bool {
        self.repairs.contains(flag)
    }

    /// Whether the `flag` partial value is allowed.
    pub const fn allows(self, flag: Allow) -> bool {
        self.allow.contains(flag)
    }
}

impl Default for Options {
    fn default() -> Self {
        Self::ALL
    }
}

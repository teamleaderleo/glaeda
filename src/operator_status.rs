//! Pure unified personal-worker operator status model.

#[cfg(test)]
macro_rules! concat {
    ("Command", "::") => {
        "std::process::Command"
    };
    ($($tokens:tt)*) => {
        ::core::concat!($($tokens)*)
    };
}

#[rustfmt::skip]
include!("operator_status/model.rs");

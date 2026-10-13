use mago_database::file::File;

/// The grammar a file is written in.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Dialect {
    /// PHP: the file starts as inline text until an opening tag.
    #[default]
    Php,
    /// PHP#: the file starts as code, with no opening tag.
    Sharp,
}

impl Dialect {
    /// Returns the dialect of the given file, chosen from its name.
    ///
    /// A file whose name ends in `.sharp` is PHP#; every other file is PHP.
    #[inline]
    #[must_use]
    pub fn of(file: &File) -> Dialect {
        if file.name.ends_with(b".sharp") { Dialect::Sharp } else { Dialect::Php }
    }

    /// Returns `true` for PHP#.
    #[inline]
    #[must_use]
    pub const fn is_sharp(self) -> bool {
        matches!(self, Dialect::Sharp)
    }

    /// Returns `true` for PHP.
    #[inline]
    #[must_use]
    pub const fn is_php(&self) -> bool {
        matches!(self, Dialect::Php)
    }
}

//! The standard library's effect classes, spec section 29, the types their members take and return, the library
//! classes their code calls, and the members of a `List`, spec section 12, each as the path a project reads it from in
//! `vendor/` and its code from `sharp/composer/library/`. Every project holds them, so the tests that check `.sharp`
//! files include this module and add the files to every project they build.

/// A file of the standard library at `path` under `sharp/composer/library/`, named as a project reads it from
/// `vendor/`.
macro_rules! library_file {
    ($path:literal) => {
        (
            concat!("vendor/heyjordanparker/php-sharp-composer/library/", $path),
            include_str!(concat!("../../../composer/library/", $path)),
        )
    };
}

/// The members of a `List`, which every collection call of a project reaches.
pub const LIST_MEMBERS: (&str, &str) = library_file!("Sharp/ListMembers.sharp");

/// The members of a `Map`, as `LIST_MEMBERS` are a `List`'s.
pub const MAP_MEMBERS: (&str, &str) = library_file!("Sharp/MapMembers.sharp");

/// The members of a `Set`, as `LIST_MEMBERS` are a `List`'s.
pub const SET_MEMBERS: (&str, &str) = library_file!("Sharp/SetMembers.sharp");

/// The lazy operations of an `Iterable`, as `LIST_MEMBERS` are a `List`'s members.
pub const ITERABLE_MEMBERS: (&str, &str) = library_file!("Sharp/IterableMembers.sharp");

pub const EFFECT_CLASSES: [(&str, &str); 31] = [
    LIST_MEMBERS,
    MAP_MEMBERS,
    SET_MEMBERS,
    ITERABLE_MEMBERS,
    library_file!("Sharp/System/Environment.sharp"),
    library_file!("Sharp/Time/Clock.sharp"),
    library_file!("Sharp/Random.sharp"),
    library_file!("Sharp/IO/Console.sharp"),
    library_file!("Sharp/System/Process.sharp"),
    library_file!("Sharp/System/ProcessResult.sharp"),
    library_file!("Sharp/IO/Files.sharp"),
    library_file!("Sharp/Net/Http.sharp"),
    library_file!("Sharp/Data/Cache.sharp"),
    library_file!("Sharp/Net/Mail.sharp"),
    library_file!("Sharp/Data/Database.sharp"),
    library_file!("Sharp/Time/Duration.sharp"),
    library_file!("Sharp/Time/Instant.sharp"),
    library_file!("Sharp/Time/DateTime.sharp"),
    library_file!("Sharp/Time/Date.sharp"),
    library_file!("Sharp/Time/TimeZone.sharp"),
    library_file!("Sharp/Time/DateTimeInterfaceMembers.sharp"),
    library_file!("Sharp/Time/Weekday.sharp"),
    library_file!("Sharp/Time/Month.sharp"),
    library_file!("Sharp/Int.sharp"),
    library_file!("Sharp/Math.sharp"),
    library_file!("Sharp/Net/HttpMethod.sharp"),
    library_file!("Sharp/Net/HttpRequest.sharp"),
    library_file!("Sharp/Net/HttpResponse.sharp"),
    library_file!("Sharp/Net/MailAddress.sharp"),
    library_file!("Sharp/Net/MailMessage.sharp"),
    library_file!("Sharp/Net/Url.sharp"),
];

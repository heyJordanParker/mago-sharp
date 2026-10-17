//! The standard library's effect classes, spec section 29, the types their members take and return, and the library
//! classes their code calls, each as the path a project reads it from in `vendor/` and its code from
//! `sharp/composer/library/`. Every project holds them, so the tests that check `.sharp` files include this module and
//! add the files to every project they build.

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

pub const EFFECT_CLASSES: [(&str, &str); 19] = [
    library_file!("Sharp/Environment.sharp"),
    library_file!("Sharp/Clock.sharp"),
    library_file!("Sharp/Random.sharp"),
    library_file!("Sharp/Console.sharp"),
    library_file!("Sharp/Process.sharp"),
    library_file!("Sharp/ProcessResult.sharp"),
    library_file!("Sharp/Files.sharp"),
    library_file!("Sharp/Http.sharp"),
    library_file!("Sharp/Cache.sharp"),
    library_file!("Sharp/Mail.sharp"),
    library_file!("Sharp/Database.sharp"),
    library_file!("Sharp/Time/Duration.sharp"),
    library_file!("Sharp/Net/HttpMethod.sharp"),
    library_file!("Sharp/Net/HttpRequest.sharp"),
    library_file!("Sharp/Net/HttpResponse.sharp"),
    library_file!("Sharp/Net/MailAddress.sharp"),
    library_file!("Sharp/Net/MailMessage.sharp"),
    library_file!("Sharp/Net/Url.sharp"),
    library_file!("Sharp/Net/Email.sharp"),
];

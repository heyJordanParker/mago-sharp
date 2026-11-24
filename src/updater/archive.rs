use std::fs;
use std::io;
use std::path::Path;

use super::error::UpdateError;

pub fn extract_file(archive_path: &Path, file_to_extract: &str, into_dir: &Path) -> Result<(), UpdateError> {
    let ext = archive_path.extension().and_then(|e| e.to_str()).unwrap_or("");

    if ext == "gz" {
        // Check for .tar.gz
        let stem = archive_path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem.ends_with(".tar") {
            return extract_tar_gz(archive_path, file_to_extract, into_dir);
        }
    }

    if ext == "zip" {
        return extract_zip(archive_path, file_to_extract, into_dir);
    }

    Err(UpdateError::Update(format!("unsupported archive format: {}", archive_path.display())))
}

fn extract_tar_gz(archive_path: &Path, file_to_extract: &str, into_dir: &Path) -> Result<(), UpdateError> {
    let file = fs::File::open(archive_path)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);

    let mut entry = archive
        .entries()?
        .filter_map(|e| e.ok())
        .find(|e| e.path().ok().is_some_and(|p| p == Path::new(file_to_extract)))
        .ok_or_else(|| UpdateError::Update(format!("could not find `{file_to_extract}` in archive")))?;

    entry.unpack_in(into_dir)?;

    Ok(())
}

fn extract_zip(archive_path: &Path, file_to_extract: &str, into_dir: &Path) -> Result<(), UpdateError> {
    let file = fs::File::open(archive_path)?;
    let mut archive = zip::ZipArchive::new(file)?;
    let mut entry = archive.by_name(file_to_extract)?;

    let output_path = into_dir.join(&*entry.name()?);
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut output = fs::File::create(output_path)?;
    io::copy(&mut entry, &mut output)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;

    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    use super::extract_file;

    #[test]
    fn a_zip_release_gives_up_only_the_binary_it_is_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let archive_path = dir.path().join("mago-0.7.2-x86_64-pc-windows-msvc.zip");
        let mut writer = ZipWriter::new(fs::File::create(&archive_path).unwrap());
        writer.start_file("mago-0.7.2-x86_64-pc-windows-msvc/README.md", SimpleFileOptions::default()).unwrap();
        writer.write_all(b"the readme").unwrap();
        writer.start_file("mago-0.7.2-x86_64-pc-windows-msvc/mago.exe", SimpleFileOptions::default()).unwrap();
        writer.write_all(b"the binary").unwrap();
        writer.finish().unwrap();
        let into_dir = dir.path().join("extracted");

        extract_file(&archive_path, "mago-0.7.2-x86_64-pc-windows-msvc/mago.exe", &into_dir).unwrap();

        assert_eq!(fs::read(into_dir.join("mago-0.7.2-x86_64-pc-windows-msvc/mago.exe")).unwrap(), b"the binary");
        assert!(!into_dir.join("mago-0.7.2-x86_64-pc-windows-msvc/README.md").exists());
    }
}

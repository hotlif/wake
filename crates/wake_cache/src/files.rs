use std::fs::File;
use std::io;
use std::path::Path;

pub(super) fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

pub(super) fn reject_special(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(invalid("cache entry is not a regular file"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(super) fn open_regular(path: &Path) -> io::Result<File> {
    reject_special(path)?;
    File::open(path)
}

pub(super) fn directories(path: &Path, create: bool) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(invalid("cache directory must be absolute"));
    }
    let ancestors: Vec<_> = path.ancestors().collect();
    for directory in ancestors.into_iter().rev() {
        let metadata = match std::fs::symlink_metadata(directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
                match std::fs::create_dir(directory) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
                std::fs::symlink_metadata(directory)?
            }
            Err(error) => return Err(error),
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(invalid(
                "cache directory contains a symlink or non-directory component",
            ));
        }
    }
    Ok(())
}

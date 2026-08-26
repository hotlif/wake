use std::path::PathBuf;
use std::sync::Arc;

use wake_common::OsFileSystem;
use wake_resolver::ResolutionEnvironment;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let issuer = PathBuf::from(args.next().expect("issuer directory is required"));
    let package = args
        .next()
        .expect("package name is required")
        .into_string()
        .expect("package name must be UTF-8");
    let environment = ResolutionEnvironment::new(Arc::new(OsFileSystem));
    match environment
        .resolver()
        .resolve_package_root(&package, &issuer)
    {
        Ok(path) => println!("OK\t{}", path.display()),
        Err(error) => println!("ERR\t{:?}\t{}", error.kind(), error),
    }
}

pub mod language;
pub mod project;

pub use language::{detect_languages, language_for_file, Language};
pub use project::{detect_project_root, find_package_roots, socket_path};

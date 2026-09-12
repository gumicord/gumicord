//! Manual smoke for the native file picker. Opens one dialog and prints
//! the choice. CI builds but never runs this: dialogs need a human.

fn main() {
    let options = gumicord_platform::file_dialog::PickOptions {
        title: Some("Pick an image".to_owned()),
        filters: vec![gumicord_platform::file_dialog::FileFilter {
            name: "Images".to_owned(),
            extensions: vec!["png".to_owned(), "jpg".to_owned()],
        }],
        starting_dir: None,
        file_name: None,
    };
    match gumicord_platform::file_dialog::pick_file(&options) {
        Ok(Some(path)) => println!("{}", path.display()),
        Ok(None) => println!("no choice"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

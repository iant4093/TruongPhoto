use std::path::Path;

fn main() {
    let dist_dir = Path::new("../../dist");
    if let Err(err) = std::fs::create_dir_all(dist_dir) {
        panic!("failed to create ../../dist directory: {err}");
    }

    let index_path = dist_dir.join("index.html");
    if !index_path.exists() {
        let placeholder = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>TruongPhoto</title>
</head>
<body>
  <div id="root"></div>
</body>
</html>
"#;
        if let Err(err) = std::fs::write(&index_path, placeholder) {
            panic!("failed to write placeholder ../../dist/index.html: {err}");
        }
    }

    println!("cargo:rerun-if-changed=../../dist");
}

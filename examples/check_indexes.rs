use std::{collections::BTreeMap, fs, path::Path};
fn load(root: &Path, dir: &Path, files: &mut BTreeMap<String, Vec<u8>>) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            load(root, &path, files)?
        } else {
            files.insert(
                path.strip_prefix(root)?
                    .to_str()
                    .unwrap()
                    .replace('\\', "/"),
                fs::read(path)?,
            );
        }
    }
    Ok(())
}
fn main() -> anyhow::Result<()> {
    let root = std::env::args().nth(1).expect("documentation directory");
    let root = Path::new(&root);
    let mut files = BTreeMap::new();
    load(root, root, &mut files)?;
    let original = files.clone();
    scrolls::render::refresh_indexes(&mut files)?;
    let changes: Vec<_> = files
        .iter()
        .filter(|(p, b)| original.get(*p) != Some(*b))
        .map(|(p, _)| p.clone())
        .collect();
    if !changes.is_empty() {
        for path in &changes {
            fs::write(
                root.join(format!("{}.expected", path.replace('/', "_"))),
                &files[path],
            )?;
        }
        anyhow::bail!("index adapter differs: {changes:?}")
    }
    println!("All six directory indexes match independently regenerated content");
    Ok(())
}

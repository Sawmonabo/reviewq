use std::fs;
use std::sync::atomic::AtomicBool;

use reviewq_domain::session::{AbsolutePath, HostProvider, NativeBinding, NativeIdentity};
use serde_json::json;

use super::claude_lead_source;

/// A Claude lead's file is the named place before Claude Code writes there, the place a
/// proving file is found, and no place once the named file holds another conversation.
#[test]
fn claude_lead_file_is_named_found_or_refused() {
    let profile = tempfile::tempdir().unwrap();
    let root = profile.path().canonicalize().unwrap();
    let absolute = |path| AbsolutePath::from_canonical(path).unwrap();
    let id = "5f69903e-32ab-4713-81c1-8b67c73ed553";
    let binding = NativeBinding::new(
        NativeIdentity::new(
            HostProvider::Claude,
            absolute(root.clone()),
            id.parse().unwrap(),
        ),
        absolute(root.join("checkout")),
    );
    let stopped = AtomicBool::new(false);
    let source = |path: &AbsolutePath| claude_lead_source(&binding, Some(path), &stopped).unwrap();
    let row = |cwd: &str| format!("{}\n", json!({"sessionId": id, "cwd": root.join(cwd)}));
    let opening = format!("{}\n", json!({"type": "mode", "sessionId": id}));

    let folder = root.join("projects/checkout");
    fs::create_dir_all(&folder).unwrap();
    let named = absolute(folder.join(format!("{id}.jsonl")));
    assert_eq!(source(&named), Some(named.clone()));
    fs::write(named.as_path(), &opening).unwrap();
    assert_eq!(source(&named), Some(named.clone()));
    fs::write(named.as_path(), format!("{opening}{}", row("elsewhere"))).unwrap();
    assert_eq!(source(&named), None);
    fs::write(named.as_path(), format!("{opening}{}", row("checkout"))).unwrap();
    assert_eq!(source(&named), Some(named.clone()));

    // A place that holds nothing gives way to the file that proves the conversation.
    let missing = absolute(root.join(format!("projects/other/{id}.jsonl")));
    assert_eq!(source(&missing), Some(named));
}

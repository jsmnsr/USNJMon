const HEADER: &str = include_str!(env!("USN_PROVIDER_HEADER"));
use super::*;
use std::cell::Cell;
use std::path::Path;

#[test]
fn native_resources_render_without_registration() {
    verify_resources(Path::new(env!("USN_PROVIDER_DLL"))).unwrap();
}

#[test]
fn disabled_provider_and_write_errors_fail_closed() {
    let written = Cell::new(false);
    assert!(
        checked_write(
            || anyhow::bail!("disabled"),
            || {
                written.set(true);
                0
            }
        )
        .is_err()
    );
    assert!(!written.get());
    assert!(checked_write(|| Ok(()), || 8).is_err());
    let checks = Cell::new(0);
    assert!(
        checked_write(
            || {
                checks.set(checks.get() + 1);
                ensure!(checks.get() == 1, "disabled after acceptance");
                Ok(())
            },
            || 0
        )
        .is_err()
    );
    assert!(checked_write(|| Ok(()), || 0).is_ok());
}

#[test]
fn descriptors_match_manifest_compiler_output() {
    let manifest =
        roxmltree::Document::parse(include_str!("..\\..\\..\\resources\\provider.man")).unwrap();
    let provider = manifest
        .descendants()
        .find(|node| node.has_tag_name("provider"))
        .unwrap();
    assert_eq!(provider.attribute("name"), Some(PROVIDER));
    assert_eq!(provider.attribute("guid"), Some(PROVIDER_GUID));
    let header: String = HEADER.chars().filter(|c| !c.is_whitespace()).collect();
    for id in schema::RECORD_IDS.into_iter().chain(101..=107) {
        let d = descriptor(id).unwrap();
        let expected = format!(
            "={{0x{:x},0x{:x},0x{:x},0x{:x},0x{:x},0x{:x},0x{:x}}}",
            d.Id, d.Version, d.Channel, d.Level, d.Opcode, d.Task, d.Keyword
        );
        assert!(
            header.to_ascii_lowercase().contains(&expected),
            "{id}: {expected}"
        );
    }
}

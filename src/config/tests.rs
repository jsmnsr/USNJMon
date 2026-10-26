use super::*;

fn config() -> Config {
    serde_json::from_str(
        r#"{
        "volumes":[{"name":"system","path":"C:\\"}],
        "poll_interval_ms":200,"event_interval_ms":2,"retry_interval_ms":10000
    }"#,
    )
    .unwrap()
}

fn explicit(config: &mut Config) -> &mut Vec<VolumeConfig> {
    let Volumes::Explicit(volumes) = &mut config.volumes else {
        panic!("expected explicit")
    };
    volumes
}

#[test]
fn valid_local_paths_and_rejected_unsafe_state_names() {
    for path in [
        r"C:\",
        r"C:\mounts\data\",
        r"\\?\Volume{00000000-0000-0000-0000-000000000001}\",
    ] {
        let mut c = config();
        explicit(&mut c)[0].path = path.into();
        c.validate().unwrap();
    }
    for name in ["", "../escape", "..", "con", "NUL", "COM1", "lpt9", "a:b"] {
        let mut c = config();
        explicit(&mut c)[0].name = name.into();
        assert!(c.validate().is_err(), "{name}");
    }
}

#[test]
fn rejects_remote_paths_duplicate_names_and_unbounded_rates() {
    for path in [r"\\server\share\", r"C:", r"relative\", "C:\\\0"] {
        let mut c = config();
        explicit(&mut c)[0].path = path.into();
        assert!(c.validate().is_err(), "{path}");
    }
    let mut c = config();
    explicit(&mut c).push(VolumeConfig {
        name: "SYSTEM".into(),
        path: r"D:\".into(),
    });
    assert!(c.validate().is_err());
    let mut c = config();
    c.event_interval_ms = 0;
    assert!(c.validate().is_err());
    assert!(serde_json::from_str::<Config>(r#"{"extra":true}"#).is_err());
}

#[test]
fn default_discovery_selects_all_and_exclusion_wins_across_aliases() {
    let a = DiscoveredVolume {
        path: r"\\?\Volume{00000000-0000-0000-0000-000000000001}\".into(),
        mounts: vec![r"C:\".into(), r"D:\mount\data\".into()],
    };
    let b = DiscoveredVolume {
        path: r"\\?\Volume{00000000-0000-0000-0000-000000000002}\".into(),
        mounts: vec![],
    };
    let all = VolumeSelectors::default()
        .select(vec![a.clone(), b.clone(), a.clone()])
        .unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].name, "volume-00000000-0000-0000-0000-000000000001");
    let selectors = VolumeSelectors {
        include: vec![],
        exclude: vec![r"d:\MOUNT\data".into()],
    };
    assert_eq!(
        selectors.select(vec![a.clone(), b.clone()]).unwrap()[0].path,
        volume_key(&b.path).unwrap()
    );
    let selectors = VolumeSelectors {
        include: vec!["c:".into(), b.path.clone()],
        exclude: vec![a.path.clone()],
    };
    assert_eq!(selectors.select(vec![a, b]).unwrap().len(), 1);
    assert!(
        VolumeSelectors {
            include: vec!["z:".into()],
            exclude: vec![]
        }
        .select(vec![])
        .is_err()
    );
    let config: Config = serde_json::from_str(
        r#"{"poll_interval_ms":200,"event_interval_ms":2,"retry_interval_ms":10000}"#,
    )
    .unwrap();
    assert!(matches!(config.volumes, Volumes::Discover(_)));
    config.validate().unwrap();
    let example: Config =
        serde_json::from_str(include_str!("..\\..\\config.example.json")).unwrap();
    example.validate().unwrap();
    assert!(serde_json::from_str::<Volumes>(r#"{"incldue":["C:\\"]}"#).is_err());
    for bad in [
        r"\\server\share",
        r"C:\..\x",
        r"C:\*",
        r"\\?\Volume{wrong}\",
    ] {
        assert!(volume_key(bad).is_err());
    }
}

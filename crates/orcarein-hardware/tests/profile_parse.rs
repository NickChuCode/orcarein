// Black-box integration tests for Profile parsing.
// Does NOT depend on a shipped arm.toml file — uses inline TOML strings.

use orcarein_hardware::{Backend, Profile, Risk, TransportKind};

const VALID_PROFILE: &str = r#"
schema_version = 1
[device]
name = "test-device"
description = "Integration test device"
transport = "i2c"
i2c_bus = 1
i2c_addr = 0x48

[[intent]]
name = "scan_bus"
description = "Scan the I2C bus"
risk = "safe"
backend = "native"
[intent.native]
op = "i2c_scan"

[[intent]]
name = "set_angle"
description = "Set servo angle"
risk = "risky"
backend = "python"
[[intent.param]]
name = "servo_id"
type = "int"
min = 0
max = 15
[[intent.param]]
name = "angle"
type = "int"
min = 0
max = 180
[intent.python]
call = "kit.servo[{servo_id}].angle = {angle}"
"#;

#[test]
fn integration_parses_valid_profile() {
    let profile = Profile::from_toml_str(VALID_PROFILE).expect("should parse valid profile");

    assert_eq!(profile.device.name, "test-device");
    assert_eq!(profile.device.transport, TransportKind::I2c);
    assert_eq!(profile.device.i2c_addr, Some(0x48));
    assert_eq!(profile.intents.len(), 2);
}

#[test]
fn integration_intent_names_and_risks() {
    let profile = Profile::from_toml_str(VALID_PROFILE).unwrap();

    assert_eq!(profile.intents[0].name, "scan_bus");
    assert_eq!(profile.intents[0].risk, Risk::Safe);
    assert!(matches!(profile.intents[0].backend, Backend::Native { .. }));

    assert_eq!(profile.intents[1].name, "set_angle");
    assert_eq!(profile.intents[1].risk, Risk::Risky);
    assert!(matches!(profile.intents[1].backend, Backend::Python { .. }));
}

#[test]
fn integration_params_correct() {
    let profile = Profile::from_toml_str(VALID_PROFILE).unwrap();
    let set_angle = &profile.intents[1];

    assert_eq!(set_angle.params.len(), 2);
    assert_eq!(set_angle.params[0].name, "servo_id");
    assert_eq!(set_angle.params[0].min, Some(0.0));
    assert_eq!(set_angle.params[0].max, Some(15.0));

    assert_eq!(set_angle.params[1].name, "angle");
    assert_eq!(set_angle.params[1].max, Some(180.0));
}

#[test]
fn integration_python_init_parsed() {
    let with_init = r#"
schema_version = 1
[device]
name = "servo-board"
description = "16-channel servo board"
transport = "i2c"
[device.python]
init = "from adafruit_servokit import ServoKit\nkit = ServoKit(channels=16)"

[[intent]]
name = "home"
description = "Move all servos to neutral"
risk = "risky"
backend = "python"
[intent.python]
call = "home()"
"#;
    let profile = Profile::from_toml_str(with_init).unwrap();
    assert!(profile.device.python_init.is_some());
    assert!(profile
        .device
        .python_init
        .as_deref()
        .unwrap()
        .contains("ServoKit"));
}

#[test]
fn integration_none_transport() {
    let minimal = r#"
schema_version = 1
[device]
name = "virtual"
description = "virtual device"
transport = "none"
[[intent]]
name = "ping"
description = "ping the device"
risk = "safe"
backend = "python"
[intent.python]
call = "ping()"
"#;
    let profile = Profile::from_toml_str(minimal).unwrap();
    assert_eq!(profile.device.transport, TransportKind::None);
}

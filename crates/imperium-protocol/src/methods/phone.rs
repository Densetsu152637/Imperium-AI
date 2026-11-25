use super::*;

/// `phone.pair`: a one-time code a phone enters to pair with this computer.
#[derive(Debug)]
pub struct PhonePair;

impl Method for PhonePair {
    const NAME: &'static str = "phone.pair";
    type Params = PhonePairParams;
    type Result = PhonePairResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PhonePairParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PhonePairResult {
    pub pairing: PairingCode,
    /// Where the phone app is served, when the phone listener is on.
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PairingCode {
    pub code: String,
    #[ts(type = "number")]
    pub expires_in_s: u64,
}

/// `phone.devices`: paired phones.
#[derive(Debug)]
pub struct PhoneDevices;

impl Method for PhoneDevices {
    const NAME: &'static str = "phone.devices";
    type Params = PhoneDevicesParams;
    type Result = PhoneDevicesResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PhoneDevicesParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PhoneDevicesResult {
    pub devices: Vec<DeviceInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    /// `read`, `approve`.
    pub scopes: Vec<String>,
    pub paired_at: String,
}

/// `phone.revoke`: unpair a phone.
#[derive(Debug)]
pub struct PhoneRevoke;

impl Method for PhoneRevoke {
    const NAME: &'static str = "phone.revoke";
    type Params = PhoneRevokeParams;
    type Result = PhoneRevokeResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PhoneRevokeParams {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PhoneRevokeResult {
    pub found: bool,
}

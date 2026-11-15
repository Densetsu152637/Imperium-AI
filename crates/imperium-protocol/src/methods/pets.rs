use super::*;

/// `pet.list`: installed pets, the selected one, and its current state. Installed pets are
/// listed even when `pets.enabled` is false; `selected` and `state` are then null.
#[derive(Debug)]
pub struct PetList;

impl Method for PetList {
    const NAME: &'static str = "pet.list";
    type Params = PetListParams;
    type Result = PetListResult;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PetListParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PetListResult {
    pub pets: Vec<PetInfo>,
    /// None when pets are off or none is installed.
    pub selected: Option<String>,
    pub state: Option<PetStateInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PetInfo {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub author: String,
    pub license: String,
    pub cell_width: u32,
    pub cell_height: u32,
    /// Animations the pet draws (a strip exists for each).
    pub drawn: Vec<String>,
    /// The package's `pet.json`, unparsed.
    pub pet_json: String,
}

/// Notification sent to every client when the pet's animation changes.
pub const PET_STATE: &str = "pet.state";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PetStateInfo {
    pub pet_id: String,
    pub animation: String,
    pub frames: u32,
    /// Per-frame durations in milliseconds.
    pub durations: Vec<u32>,
    pub looping: bool,
    /// When the animation started, so every client draws it in phase.
    #[ts(type = "number")]
    pub since_unix_ms: u64,
    pub hidden: bool,
}

/// `pet.strip`: a pet's strip image for an animation. Works without the pet engine, but then
/// `pet_id` is required (there is no selected pet).
#[derive(Debug)]
pub struct PetStrip;

impl Method for PetStrip {
    const NAME: &'static str = "pet.strip";
    type Params = PetStripParams;
    type Result = PetStripResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PetStripParams {
    pub animation: String,
    /// The pet to draw; the selected pet when absent.
    #[serde(default)]
    pub pet_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PetStripResult {
    /// The strip PNG, base64-encoded.
    pub png_base64: String,
}

/// `pet.interact`: the user patted or clicked the pet.
#[derive(Debug)]
pub struct PetInteract;

impl Method for PetInteract {
    const NAME: &'static str = "pet.interact";
    type Params = PetInteractParams;
    type Result = PetInteractResult;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PetInteractParams {
    /// `pat`, `hide`, or `show`.
    pub kind: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PetInteractResult {}

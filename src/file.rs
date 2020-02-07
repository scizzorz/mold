/// This module contains all the associated structs for serializing and
/// deserializing a moldfile.
use indexmap::IndexMap;
use indexmap::IndexSet;
use serde_derive::Deserialize;
use serde_derive::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

// sorted by insertion order
pub type IncludeVec = Vec<Remote>;
pub type TargetSet = IndexSet<String>;
pub type VarMap = IndexMap<String, String>; // TODO maybe down the line this should allow nulls to `unset` a variable
pub type EnvMap = IndexMap<String, VarMap>;

// sorted alphabetically
pub type RecipeMap = BTreeMap<String, Recipe>; // sorted alphabetically

pub const DEFAULT_FILES: &[&str] = &["mold.yaml", "mold.yml", "moldfile", "Moldfile"];

fn default_recipe_dir() -> PathBuf {
  "./mold".into()
}

fn default_git_ref() -> String {
  "master".into()
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Moldfile {
  /// Version of mold required to run this Moldfile
  pub version: String,

  /// The directory that recipe scripts can be found in
  #[serde(default = "default_recipe_dir")]
  pub recipe_dir: PathBuf,

  /// A map of includes
  #[serde(default)]
  pub includes: IncludeVec,

  /// A map of recipes
  #[serde(default)]
  pub recipes: RecipeMap,

  /// A list of environment variables used to parametrize recipes
  ///
  /// BREAKING: Renamed from `environment` in 0.3.0
  #[serde(default)]
  pub variables: VarMap,

  /// A map of environment names to variable maps used to parametrize recipes
  ///
  /// ADDED: 0.3.0
  #[serde(default)]
  pub environments: EnvMap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Remote {
  /// Git URL of a remote repo
  pub url: String,

  /// Git ref to keep up with
  #[serde(rename = "ref", default = "default_git_ref")]
  pub ref_: String,

  /// Moldfile to look at
  pub file: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecipeBase {
  /// A short description of the module's contents
  #[serde(default)]
  pub help: String,

  /// A list of environment variables that overrides the base environment
  ///
  /// BREAKING: 0.3.0: Renamed from `environment`
  /// BREAKING: 0.5.0: Functionality changed from a map of (key, value) pairs to
  /// a map of (key, description) pairs for documentation.
  #[serde(default)]
  pub variables: VarMap,

  // A map of environment names to variable maps used to parametrize recipes
  //
  // ADDED: 0.3.0
  // REMOVED: 0.5.0
  // pub environments: EnvMap,
  /// The working directory relative to the calling Moldfile's root_dir
  ///
  /// ADDED: 0.4.0
  #[serde(default)]
  pub work_dir: Option<PathBuf>,

  /// The actual search_dir of this recipe
  ///
  /// This is used for Includes, where the command may be lifted up to the
  /// top-level, but the search_dir is located in a different location
  #[serde(skip)]
  pub search_dir: Option<PathBuf>,

  /// The module path that led to this recipe existing
  ///
  /// This is used for explanations as well as creating the environment
  /// variables.
  #[serde(skip)]
  pub mod_list: Vec<Module>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Recipe {
  // apparently the order here matters?
  Shell(Shell),
  Command(Command),
  Module(Module),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Module {
  /// Base data
  #[serde(flatten)]
  pub base: RecipeBase,

  /// Remote data
  #[serde(flatten)]
  pub remote: Remote,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shell {
  /// Base data
  #[serde(flatten)]
  pub base: RecipeBase,

  /// A list of pre-execution dependencies
  #[serde(default)]
  pub deps: Vec<String>,

  /// The command to pass to $SHELL to execute this recipe
  ///
  /// eg: "bash $MOLD_ROOT/foo.sh"
  pub shell: String,

  /// The script contents as a multiline string
  pub script: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
  /// Base data
  #[serde(flatten)]
  pub base: RecipeBase,

  /// A list of pre-execution dependencies
  #[serde(default)]
  pub deps: Vec<String>,

  /// A list of command arguments
  pub command: Vec<String>,
}

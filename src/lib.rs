use failure::Error;
use serde_derive::Deserialize;
use serde_derive::Serialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

pub mod remote;

pub type RecipeMap = BTreeMap<String, Recipe>;
pub type TypeMap = BTreeMap<String, Type>;

#[derive(Debug, Serialize, Deserialize)]
pub struct Moldfile {
  /// The directory that recipe scripts can be found in
  #[serde(default = "default_recipe_dir")]
  pub recipe_dir: String,

  /// A map of recipes.
  pub recipes: RecipeMap,

  /// A map of interpreter types and characteristics.
  pub types: TypeMap,
}

fn default_recipe_dir() -> String {
  "./recipes".to_string()
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Recipe {
  Group(Group),
  Script(Script),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Group {
  /// Git URL of a remote repo
  pub url: String,

  /// Git ref to keep up with
  #[serde(alias = "ref", default = "default_git_ref")]
  pub ref_: String,
}

fn default_git_ref() -> String {
  "origin/master".to_string()
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Script {
  /// Which interpreter should be used to execute this script.
  #[serde(alias = "type")]
  pub type_: String,

  /// A short description of the command.
  pub help: Option<String>,

  /// The script file name.
  ///
  /// If left undefined, Mold will attempt to discover the recipe name by
  /// searching the recipe_dir for any files that start with the recipe name and
  /// have an appropriate extension for the specified interpreter type.
  pub script: Option<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Type {
  /// A list of arguments used as a shell command.
  ///
  /// Any element "?" will be / replaced with the desired script when
  /// executing. eg:
  ///   ["python", "-m", "?"]
  /// will produce the shell command when .exec("foo") is called:
  ///   $ python -m foo
  pub command: Vec<String>,

  /// A list of extensions used to search for the script name.
  ///
  /// These should omit the leading dot.
  #[serde(default = "default_extensions")]
  pub extensions: Vec<String>,
}

impl Type {
  /// Execute a file using self.command
  pub fn exec(&self, path: &Path) -> Result<(), Error> {
    let mut args = self.command.clone();
    let command = args.remove(0);

    // replace "?" with the script name
    let args: Vec<_> = args
      .iter()
      .map(|x| if x == "?" { path.to_str().unwrap() } else { x })
      .collect();

    let exit_status = Command::new(&command)
      .args(&args[..])
      .spawn()
      .and_then(|mut handle| handle.wait())?;

    if !exit_status.success() {
      return Err(failure::err_msg("recipe exited with non-zero code"));
    }

    Ok(())
  }

  /// Attempt to discover an appropriate script in a recipe directory.
  pub fn find(&self, dir: &Path, name: &str) -> Result<PathBuf, Error> {
    // set up the pathbuf to look for dir/name
    let mut pb = dir.to_path_buf();
    pb.push(name);

    // try all of our known extensions, early returning on the first match
    for ext in &self.extensions {
      pb.set_extension(ext);
      if pb.is_file() {
        return Ok(pb);
      }
    }
    Err(failure::err_msg("Couldn't find a file"))
  }
}

fn default_extensions() -> Vec<String> {
  return vec![];
}

mod cargo;
pub mod lang;
pub mod remote;
pub mod util;

use colored::*;
use failure::Error;
use indexmap::indexmap;
use indexmap::IndexMap;
use indexmap::IndexSet;
use remote::Remote;
use semver::Version;
use semver::VersionReq;
use std::collections::BTreeMap;
use std::fs;
use std::io::prelude::*;
use std::path::Path;
use std::path::PathBuf;
use std::process;
use std::string::ToString;

// sorted by insertion order
pub type IncludeVec = Vec<Include>;
pub type TargetSet = IndexSet<String>;
pub type EnvSet = IndexSet<String>;
pub type VarMap = IndexMap<String, String>; // TODO maybe down the line this should allow nulls to `unset` a variable
pub type SourceMap = IndexMap<String, PathBuf>;

// sorted alphabetically
pub type RecipeMap = BTreeMap<String, Recipe>;

/// Complete set of application state
pub struct Mold {
    /// A set of currently active environments
    pub envs: EnvSet,

    /// A map of recipes
    pub recipes: RecipeMap,

    /// A map of recipe sources
    pub sources: SourceMap,

    /// A map of environment variables
    pub vars: VarMap,

    /// List of Remotes that have been imported
    pub remotes: Vec<Remote>,

    /// Root of the origin moldfile
    pub root_dir: PathBuf,

    /// Path to cloned repos and generated scripts
    pub mold_dir: PathBuf,

    /// Working directory
    ///
    /// This is overridden by a recipe's `dir`
    pub work_dir: Option<String>,

    /// Use external git binary rather than libgit2
    pub use_git: bool,

    /// Skip variables when compiling moldfiles
    pub use_vars: bool,
}

/// An external module included for reuse
pub struct Include {
    /// Remote to include
    pub remote: Remote,

    /// Prefix to prepend
    pub prefix: String,
}

/// A single task to execute
#[derive(Clone)]
pub struct Recipe {
    /// A short description of the recipe
    pub help: Option<String>,

    /// Working directory relative to $MOLD_ROOT
    pub dir: Option<String>,

    /// The command to execute
    pub commands: Vec<String>,

    /// A list of prerequisite recipes
    pub requires: TargetSet,
}

/// Data straight from a file
pub struct Moldfile {
    /// Required version to load this moldfile
    pub version: String,

    /// A list of imported moldfiles
    pub includes: IncludeVec,

    /// A list of recipes
    pub recipes: RecipeMap,

    /// A list of environment variables
    pub vars: VarMap,

    /// Working directory relative to $MOLD_ROOT
    ///
    /// This is overridden by a recipe's `dir`
    pub dir: Option<String>,
}

impl Mold {
    /// Create a new, empty application and import the given path into it
    pub fn init(
        path: &Path,
        envs: Vec<String>,
        use_git: bool,
        use_vars: bool,
    ) -> Result<Mold, Error> {
        let root_dir = path.parent().unwrap_or(&Path::new("/")).to_path_buf();
        let mold_dir = root_dir.join(".mold");

        if !mold_dir.is_dir() {
            fs::create_dir(&mold_dir).map_err(|err| {
                failure::format_err!(
                    "Could not create directory {}: {}",
                    mold_dir.display().to_string().red(),
                    err
                )
            })?;
        }

        let vars = indexmap! {
          "MOLD_ROOT".into() => root_dir.to_string_lossy().into(),
          "MOLD_DIR".into() => mold_dir.to_string_lossy().into(),
        };

        let envs = envs.into_iter().collect();

        let root_dir = fs::canonicalize(&root_dir).map_err(|err| {
            failure::format_err!(
                "Couldn't canonicalize directory {}: {}",
                root_dir.display().to_string().red(),
                err
            )
        })?;

        let mold_dir = fs::canonicalize(&mold_dir).map_err(|err| {
            failure::format_err!(
                "Couldn't canonicalize directory {}: {}",
                mold_dir.display().to_string().red(),
                err
            )
        })?;

        let mut mold = Mold {
            root_dir,
            mold_dir,
            recipes: RecipeMap::new(),
            sources: SourceMap::new(),
            remotes: vec![],
            work_dir: None,
            envs,
            vars,
            use_git,
            use_vars,
        };

        mold.open(path, "")?;

        Ok(mold)
    }

    /// Delete all cloned top-level targets
    pub fn clean_all(path: &Path) -> Result<(), Error> {
        let root_dir = path.parent().unwrap_or(&Path::new("/")).to_path_buf();
        let mold_dir = root_dir.join(".mold");

        if mold_dir.is_dir() {
            fs::remove_dir_all(&mold_dir).map_err(|err| {
                failure::format_err!(
                    "Couldn't remove directory {}: {}",
                    mold_dir.display().to_string().red(),
                    err
                )
            })?;

            println!("{:>12} {}", "Deleted".red(), mold_dir.display());
        } else {
            println!("{:>12}", "Clean!".green());
        }

        Ok(())
    }

    /// Given a path, load the file into the current application
    fn open(&mut self, path: &Path, prefix: &str) -> Result<(), Error> {
        let mut file = fs::File::open(path).map_err(|err| {
            failure::format_err!(
                "Couldn't open {}: {}",
                path.display().to_string().red(),
                err
            )
        })?;

        let mut contents = String::new();
        file.read_to_string(&mut contents).map_err(|err| {
            failure::format_err!(
                "Couldn't read {}: {}",
                path.display().to_string().red(),
                err
            )
        })?;

        let data = self::lang::compile(&contents, self).map_err(|err| {
            failure::format_err!(
                "Couldn't compile {}: {}",
                path.display().to_string().red(),
                err
            )
        })?;

        let root_dir = path.parent().unwrap_or(&Path::new("/")).to_path_buf();

        // check version requirements
        let self_version = Version::parse(clap::crate_version!())?;
        let target_version = VersionReq::parse(&data.version).map_err(|err| {
            failure::format_err!(
                "Couldn't parse version requirement {} from {}: {}",
                data.version.red(),
                path.display().to_string().red(),
                err
            )
        })?;

        if !target_version.matches(&self_version) {
            return Err(failure::format_err!(
                "{} requires version {}, but mold version is {}",
                path.to_str().unwrap().blue(),
                target_version.to_string().green(),
                self_version.to_string().red()
            ));
        }

        for (name, recipe) in data.recipes {
            let new_key = format!("{}{}", prefix, name);

            // clone this recipe and prefix all of its dependencies
            let mut new_recipe = recipe.clone();
            new_recipe.requires = new_recipe
                .requires
                .iter()
                .map(|x| format!("{}{}", prefix, x))
                .collect();

            self.recipes.entry(new_key.clone()).or_insert(new_recipe);

            // keep track of where this recipe came from so it can use things from its repo
            self.sources.entry(new_key).or_insert(root_dir.clone());
        }

        for include in data.includes {
            if !include.remote.exists(&self.mold_dir) {
                include
                    .remote
                    .pull(&self.mold_dir, self.use_git)
                    .map_err(|err| {
                        failure::format_err!("Couldn't clone {}: {}", include.remote.url.red(), err)
                    })?;

                include
                    .remote
                    .checkout(&self.mold_dir, self.use_git)
                    .map_err(|err| {
                        failure::format_err!(
                            "Couldn't checkout {}: {}",
                            include.remote.ref_.red(),
                            err
                        )
                    })?;
            }

            let path = include.remote.path(&self.mold_dir);
            self.remotes.push(include.remote.clone());
            let filepath = Self::discover(&path, include.remote.file)?;
            self.open(&filepath, &include.prefix)?;
        }

        self.vars.extend(data.vars);

        // if this file has a `dir` stmt, it overrides any other dir that was set
        if let Some(rel_path) = data.dir {
            self.work_dir = Some(rel_path);
        }

        Ok(())
    }

    /// Try to find a file by walking up the tree
    ///
    /// Absolute paths will either be located or fail instantly. Relative paths
    /// will walk the entire file tree up to root, looking for a file with the
    /// given name.
    fn discover_file(name: &Path) -> Result<PathBuf, Error> {
        log::debug!("Discovering file {}", name.display());

        // if it's an absolute path, we don't need to walk up the tree.
        if name.is_absolute() {
            if name.is_file() {
                return Ok(name.to_path_buf());
            } else if name.exists() {
                let name = format!("{}", name.display());
                return Err(failure::format_err!(
                    "{} exists, but is not a file",
                    name.red()
                ));
            } else {
                let name = format!("{}", name.display());
                return Err(failure::format_err!("{} does not exist", name.red()));
            }
        }

        // walk up the tree until we find the file or hit the root
        let mut path = std::env::current_dir()
            .map_err(|err| failure::format_err!("Couldn't identify working dir: {}", err))?;

        log::debug!("Checking {}", path.join(name).display());
        while !path.join(name).is_file() {
            path.pop();
            if path.parent().is_none() {
                break;
            }
            log::debug!("Checking {}", path.join(name).display());
        }

        path.push(name);

        if path.is_file() {
            Ok(path)
        } else {
            let name = format!("{}", name.display());
            Err(failure::format_err!("Couldn't discover {}", name.red()))
        }
    }

    /// Search a directory for default moldfile
    fn discover_dir(name: &Path) -> Result<PathBuf, Error> {
        log::debug!("Discovering directory {}", name.display());
        let path = name.join("moldfile");
        Self::discover_file(&path)
    }

    /// Try to locate a file or a directory, opening it if found
    pub fn discover(dir: &Path, file: Option<PathBuf>) -> Result<PathBuf, Error> {
        // I think this should take Option<&Path> but I couldn't figure out how to
        // please the compiler when I have an existing Option<PathBuf>, so... I'm
        // just using .clone() on it.
        match file {
            Some(file) => Self::discover_file(&dir.join(file)),
            None => Self::discover_dir(dir),
        }
    }

    /// Look up a recipe by name
    fn recipe(&self, name: &str) -> Result<&Recipe, Error> {
        self.recipes
            .get(name)
            .ok_or_else(|| failure::format_err!("Couldn't find recipe {}", name.red()))
    }

    /// Construct a Task instance from a recipe name
    fn build_task(&self, name: &str) -> Result<Task, Error> {
        let recipe = self.recipe(name)?;

        // expand all variables
        let mut vars = VarMap::new();
        for (name, value) in &self.vars {
            vars.insert(name.clone(), self.expand(value, &vars).into());
        }

        // insert var for where this recipe's moldfile lives
        if let Some(source) = self.sources.get(name) {
            vars.insert("MOLD_SOURCE".into(), source.to_string_lossy().into());
        } else {
            return Err(failure::format_err!(
                "Couldn't find source repository for {}",
                name.red()
            ));
        }

        // select the recipe's working dir if it's defined, otherwise select the Mold's working dir. in
        // both cases, we want to expand the variables afterwards and join it with $MOLD_ROOT. if
        // neither dir is defined, the command will default to the current working dir.
        let work_dir = recipe
            .dir
            .clone()
            .or_else(|| self.work_dir.clone())
            .map(|raw_path| {
                self.root_dir
                    .join(self.expand(&raw_path, &vars).to_string())
            });

        // build the command strings to execute
        let mut commands = vec![];
        for command_str in &recipe.commands {
            let args = self.build_args(command_str, &vars)?;
            if args.is_empty() {
                continue;
            }
            commands.push(args);
        }

        Ok(Task {
            name: name.into(),
            commands,
            vars,
            work_dir,
        })
    }

    /// Construct and execute a Task from a recipe name
    pub fn execute(&self, name: &str) -> Result<(), Error> {
        let task = self.build_task(name)?;
        task.execute()
    }

    /// Perform variable expansion on a string
    fn expand<'a>(&self, val: &'a str, vars: &VarMap) -> std::borrow::Cow<'a, str> {
        shellexpand::env_with_context_no_errors(val, |name| {
            vars.get(name)
                .map(std::string::ToString::to_string)
                .or_else(|| std::env::var(name).ok())
                .or_else(|| Some("".into()))
        })
    }

    /// Perform variable expansion on a string and return a list of arguments to
    /// pass to std::process::Command
    fn build_args(&self, command: &str, vars: &VarMap) -> Result<Vec<String>, Error> {
        let expanded = self.expand(command, vars);
        Ok(shell_words::split(&expanded).map_err(|err| {
            failure::format_err!("Couldn't shell split string {}: {}", expanded.red(), err)
        })?)
    }

    /// Find *all* dependencies for a given set of target recipes
    pub fn find_all_dependencies(&self, targets: &TargetSet) -> Result<TargetSet, Error> {
        let mut new_targets = TargetSet::new();

        // FIXME this might not break on weird infinite cycles
        // ...but since those shouldn't happen in sanely written moldfiles...
        for name in targets {
            new_targets.extend(self.find_dependencies(name)?);
            new_targets.insert(name.clone());
        }

        Ok(new_targets)
    }

    /// Find all recipes for a *single* target recipe
    fn find_dependencies(&self, name: &str) -> Result<TargetSet, Error> {
        let recipe = self.recipe(name)?;
        let deps = recipe.requires.iter().map(ToString::to_string).collect();
        self.find_all_dependencies(&deps)
    }

    /// Update (ie: fetch + force checkout) all remotes
    pub fn update_all(&self) -> Result<(), Error> {
        for remote in &self.remotes {
            let path = remote.path(&self.mold_dir);
            if path.is_dir() {
                remote
                    .checkout(&self.mold_dir, self.use_git)
                    .map_err(|err| {
                        failure::format_err!("Couldn't checkout {}: {}", remote.ref_.red(), err)
                    })?;
            }
        }

        Ok(())
    }

    /// Print a short description of all recipes in this moldfile
    pub fn help(&self) -> Result<(), Error> {
        for (name, recipe) in &self.recipes {
            let help_str = match &recipe.help {
                Some(x) => x,
                None => "",
            };
            println!("{:>12} {}", name.cyan(), help_str);

            // print dependencies
            let deps: Vec<_> = recipe.requires.iter().map(|x| x.to_string()).collect();
            if !deps.is_empty() {
                println!("             ⮡ {}", deps.join(" ").cyan());
            }
        }

        Ok(())
    }

    /// Print a long description of a recipe
    pub fn explain(&self, name: &str) -> Result<(), Error> {
        // print recipe information
        let recipe = self.recipe(name)?;

        println!("{}", name.cyan());
        if let Some(help) = &recipe.help {
            if !help.is_empty() {
                println!("{}", help);
            }
        }

        if !recipe.requires.is_empty() {
            let deps: Vec<_> = recipe.requires.iter().map(|x| x.to_string()).collect();
            println!("{} {}", "depends on:".white(), deps.join(" ").cyan());
        }

        if let Some(dir) = &recipe.dir {
            println!("{} {}", "working dir:".white(), dir.cyan());
        }

        if !recipe.commands.is_empty() {
            println!("{}", "commands:".white());
            for command in &recipe.commands {
                println!("  {} {}", "$".white(), command);
            }
        }

        // print task information
        let task = self.build_task(name)?;

        if !task.vars.is_empty() {
            println!("{}", "variables:".white());
            for (key, val) in &task.vars {
                println!("  {} = {}", format!("${}", key).bright_cyan(), val);
            }
        }

        if !task.commands.is_empty() {
            println!("{}", "executes:".white());
            for args in &task.commands {
                println!("  {} {}", "$".green(), shell_words::join(args));
            }
        }

        println!();

        Ok(())
    }

    /// Print all variables in a shell format
    pub fn sh_vars(&self) -> Result<(), Error> {
        // expand all variables
        // expanded values are stored in this map so they can be used in later expansions
        let mut vars = VarMap::new();
        for (name, value) in &self.vars {
            let expanded_value = self.expand(value, &vars);
            println!("export {}={}", name, shell_words::quote(&expanded_value));
            vars.insert(name.clone(), expanded_value.into());
        }

        Ok(())
    }
}

/// An instantiation of a recipe ready for execution
struct Task {
    name: String,
    commands: Vec<Vec<String>>,
    work_dir: Option<PathBuf>,
    vars: VarMap,
}

impl Task {
    /// Populate a std::process::Command and spawn it
    fn execute(self) -> Result<(), Error> {
        for args in &self.commands {
            if args.is_empty() {
                continue;
            }

            let mut command = process::Command::new(&args[0]);
            command.args(&args[1..]);
            command.envs(&self.vars);

            if let Some(dir) = &self.work_dir {
                command.current_dir(dir);
            }

            println!(
                "{} {} {} {}",
                "mold".white(),
                self.name.cyan(),
                "$".green(),
                shell_words::join(args),
            );

            use std::io::ErrorKind;
            let exit_status = command
                .spawn()
                .and_then(|mut handle| handle.wait())
                .map_err(|err| match err.kind() {
                    ErrorKind::NotFound => failure::format_err!(
                        "Recipe {} failed because command {} was not found",
                        self.name.red(),
                        args[0].red()
                    ),

                    ErrorKind::PermissionDenied => failure::format_err!(
                        "Recipe {} failed because you do not have permission to execute command {}",
                        self.name.red(),
                        args[0].red()
                    ),

                    _ => failure::format_err!(
                        "Recipe {} failed due to an unknown OS error: {}",
                        self.name.red(),
                        err
                    ),
                })?;

            if !exit_status.success() {
                return Err(failure::format_err!(
                    "Recipe {} returned non-zero exit status",
                    self.name.red()
                ));
            }
        }

        Ok(())
    }
}

use super::cargo::with_authentication;
use super::util;
use colored::*;
use failure::Error;
use git2::build::CheckoutBuilder;
use git2::build::RepoBuilder;
use git2::FetchOptions;
use git2::RemoteCallbacks;
use git2::Repository;
use spinners::Spinner;
use spinners::Spinners;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;
use std::string::ToString;
use url::Url;

fn with_spinner<F>(label: String, f: F) -> Result<(), Error>
where
    F: FnOnce() -> Result<(), Error>,
{
    // spinners don't work on Windows or without a TTY
    if atty::is(atty::Stream::Stdout) && std::env::consts::FAMILY != "windows" {
        let spinner = Spinner::new(Spinners::Dots, label);
        let res = f();
        spinner.stop();
        println!();
        res
    } else {
        // without a spinner, just print the line.
        println!("{}", label);
        f()
    }
}

/// Clone a git repository
fn pull(url: &str, path: &Path) -> Result<(), Error> {
    let config = git2::Config::open_default()?;

    with_authentication(url, &config, |creds| {
        // start spinner
        log::info!("git clone {} {}", url, path.display());
        let label = format!(
            "{} {} into {}...",
            "Cloning".green(),
            url.yellow(),
            path.display().to_string().yellow()
        );

        with_spinner(label, || {
            // prep callbacks
            let mut callbacks = RemoteCallbacks::new();
            callbacks.credentials(creds);
            let mut fetch = FetchOptions::new();
            fetch.remote_callbacks(callbacks);

            // clone repo
            RepoBuilder::new().fetch_options(fetch).clone(url, path)?;
            Ok(())
        })
    })
}

fn checkout(path: &Path, ref_: &str) -> Result<(), Error> {
    let config = git2::Config::open_default()?;

    // FIXME does this matter that it's got no URL?
    with_authentication("", &config, |creds| {
        // start spinner
        let label = format!(
            "{} {} to {}...",
            "Updating".green(),
            path.display().to_string().yellow(),
            ref_.yellow()
        );

        with_spinner(label, || {
            // locate existing repo
            let repo = Repository::discover(path)?;
            let mut remote = repo.find_remote("origin")?;

            // prep callbacks
            let mut callbacks = RemoteCallbacks::new();
            callbacks.credentials(creds);
            let mut fetch = FetchOptions::new();
            fetch.remote_callbacks(callbacks);

            // fetch ref
            remote.fetch(&[ref_], Some(&mut fetch), None)?;

            // checkout the appropriate ref
            let tag_name = format!("tags/{}", ref_);
            let branch_name = format!("origin/{}", ref_);
            let object = repo
                .revparse_single(&tag_name)
                .or_else(|_| repo.revparse_single(&branch_name))?;
            repo.set_head_detached(object.id())?;

            // force checkout
            let mut checkout = CheckoutBuilder::new();
            checkout.force();
            repo.checkout_head(Some(&mut checkout))?;

            Ok(())
        })
    })
}

#[derive(Debug, Clone)]
pub struct Remote {
    /// Git URL of a remote repo
    pub url: String,

    /// Git ref to keep up with
    pub ref_: String,

    /// Moldfile to look at
    pub file: Option<PathBuf>,
}

impl Remote {
    /// Return this module's folder name in the format hash(url@ref)
    fn folder_name(&self) -> String {
        // first attempt to parse with an implicit https://
        let url = Url::parse(&format!("https://{}", &self.url)).or_else(|_| Url::parse(&self.url));
        let last_path = match url {
            Ok(ref url) => url.path_segments().map(|mut x| x.next_back()).flatten(),
            _ => None,
        };

        let hash = util::hash_url_ref(&self.url, &self.ref_);

        // not sure what kinda URLs the above will fail on, but... it can I guess.
        match last_path {
            Some(name) => format!("{}-{}-{}", name, self.ref_, hash),
            None => format!("unknown-{}-{}", self.ref_, hash),
        }
    }

    pub fn path(&self, mold_dir: &Path) -> PathBuf {
        mold_dir.join(self.folder_name())
    }

    pub fn exists(&self, mold_dir: &Path) -> bool {
        self.path(mold_dir).is_dir()
    }

    pub fn pull(&self, mold_dir: &Path) -> Result<(), Error> {
        let path = self.path(mold_dir);
        // first attempt to pull with an implicit https://
        pull(&format!("https://{}", self.url), &path).or_else(|_| pull(&self.url, &path))
    }

    pub fn checkout(&self, mold_dir: &Path) -> Result<(), Error> {
        let path = self.path(mold_dir);
        checkout(&path, &self.ref_)
    }

    /// Parse a string into an Remote
    ///
    /// The format is roughly: url[#[ref][/file]], eg:
    ///   https://foo.com/mold.git -> ref = master, file = None
    ///   https://foo.com/mold.git#dev -> ref = dev, file = None
    ///   https://foo.com/mold.git#dev/dev.yaml, ref = dev, file = dev.yaml
    ///   https://foo.com/mold.git#/dev.yaml -> ref = master, file = dev.yaml
    fn parse(url: &str) -> Self {
        match url.find('#') {
            Some(idx) => {
                let (url, frag) = url.split_at(idx);
                let frag = frag.trim_start_matches('#');

                let (ref_, file) = match frag.find('/') {
                    Some(idx) => {
                        let (ref_, file) = frag.split_at(idx);
                        let file = file.trim_start_matches('/');

                        let ref_ = match ref_ {
                            "" => "master".into(),
                            _ => ref_.into(),
                        };

                        (ref_, Some(file.into()))
                    }
                    None => (frag.into(), None),
                };

                Self {
                    url: url.into(),
                    ref_,
                    file,
                }
            }
            None => Self {
                url: url.into(),
                ref_: "master".into(),
                file: None,
            },
        }
    }
}

impl ToString for Remote {
    fn to_string(&self) -> String {
        if let Some(file) = &self.file {
            format!("{}#{}/{}", self.url, self.ref_, file.display())
        } else {
            format!("{}#{}", self.url, self.ref_)
        }
    }
}

impl FromStr for Remote {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::parse(s))
    }
}

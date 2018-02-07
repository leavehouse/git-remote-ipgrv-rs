use git2;
use ipld_git;
use lmdb;
use multihash;
use std::collections::VecDeque;
use std::env;
use std::fs;
use std::io;

use ipfs_api;

mod tracker;

#[derive(Debug)]
pub enum Error {
    ApiError(ipfs_api::Error),
    EnvVarError(env::VarError),
    Git2Error(git2::Error),
    IoError(io::Error),
    LmdbError(lmdb::Error),
    IpldGitError(ipld_git::Error),
    MultihashError(multihash::Error),
}

impl From<env::VarError> for Error {
    fn from(e: env::VarError) -> Self {
        Error::EnvVarError(e)
    }
}

impl From<git2::Error> for Error {
    fn from(e: git2::Error) -> Self {
        Error::Git2Error(e)
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::IoError(e)
    }
}

impl From<lmdb::Error> for Error {
    fn from(e: lmdb::Error) -> Self {
        Error::LmdbError(e)
    }
}

impl From<multihash::Error> for Error {
    fn from(e: multihash::Error) -> Self {
        Error::MultihashError(e)
    }
}

pub struct Helper {
    repo: git2::Repository,
    tracker: tracker::Tracker,
}

impl Helper {
    pub fn new() -> Result<Helper, Error> {
        let repo = git2::Repository::open_from_env()?;

        let mut db_path = env::var("GIT_DIR")?;
        // TODO: for windows, convert to std::path::Path, use join(), convert back to string?
        db_path.push_str("/ipgrv");
        fs::create_dir_all(&db_path)?;
        debug!("Helper::new(), db_path = {}", &db_path);
        let tracker = tracker::Tracker::new(&db_path)?;

        Ok(Helper {
            repo: repo,
            tracker: tracker,
        })
    }

    pub fn list(&self) -> Result<Vec<String>, Error> {
        let mut refs = Vec::new();
        let local_branches = self.repo.branches(Some(git2::BranchType::Local))?;
        for branch_result in local_branches {
            let (branch, _) = branch_result?;
            refs.push(format!("? {}", branch.get()
                                            .name()
                                            .expect("Branch name is not utf-8")));
        }

        let head_ref = self.repo.find_reference("HEAD")?;
        let head_ref_type = head_ref.kind()
                                .expect("HEAD ref type is unknown");
        let head_target = match head_ref_type {
            git2::ReferenceType::Oid => {
                debug!("    head is oid");
                format!("{}", head_ref.target().unwrap())
            },
            git2::ReferenceType::Symbolic =>
                head_ref.symbolic_target()
                        .expect("HEAD symbolic target is not utf-8")
                        .to_string(),
        };

        refs.push(format!("{} HEAD", head_target));
        Ok(refs)
    }

    // `src` is the local ref being pushed, `dest` is the remote ref?
    pub fn push(&self, src: &str, dest: &str, force: bool) -> Result<(), Error> {
        // get reference associated with `src`, then get src's hash
        let src_ref = self.repo.find_reference(src)?.resolve()?;
        let src_hash: git2::Oid = src_ref.target().unwrap();
        debug!("    pushing, hash = {}", src_hash);

        let mut push_helper = PushHelper::new(&self.repo);
        push_helper.push(src_hash)?;

        // TODO: the go version invokes `Tracker.SetRef` here, look more closely
        // at that
        unimplemented!();
    }
}

struct PushHelper<'a> {
    queue: VecDeque<git2::Oid>,
    repo: &'a git2::Repository
}

impl<'a> PushHelper<'a> {
    fn new(repo: &'a git2::Repository) -> PushHelper<'a> {
        PushHelper { queue: VecDeque::new(), repo: repo }
    }

    fn push(&mut self, hash: git2::Oid) -> Result<(), Error>{
        self.queue.push_back(hash);
        self.push_queue()
    }

    // push each of the objects in the queue into IPFS (as IPLD).
    fn push_queue(&mut self) -> Result<(), Error> {
        while let Some(oid) = self.queue.pop_front() {
            debug!("    pushing oid = {}", oid);
            // TODO: check tracker for src_hash.
            // if it exists, return, because theres no need to push
            // else, push
            // TODO: set `dest` to `src's hash in the tracekr
            //
            let obj_bytes = self.push_object(oid)?;

            // TODO: parse CID, add to tracker

            self.enqueue_links(&obj_bytes)?;
        }
        Ok(())
    }

    // Push git object into ipfs, returning the vector of bytes of the raw git
    // object.
    fn push_object(&mut self, oid: git2::Oid) -> Result<Vec<u8>, Error> {
        // read the git object into memory
        let odb = self.repo.odb()?;
        let odb_obj = odb.read(oid)?;
        let raw_obj = odb_obj.data();

        let mut full_obj = Vec::with_capacity(raw_obj.len() + 12);
        match odb_obj.kind() {
            git2::ObjectType::Blob => full_obj.extend_from_slice(b"blob "),
            git2::ObjectType::Tree => full_obj.extend_from_slice(b"tree "),
            git2::ObjectType::Commit => full_obj.extend_from_slice(b"commit "),
            git2::ObjectType::Tag => full_obj.extend_from_slice(b"tag "),
            _ => unimplemented!(),
        }
        full_obj.extend_from_slice(format!("{}", raw_obj.len()).as_bytes());
        full_obj.push(0);
        full_obj.extend_from_slice(raw_obj);

        // `put` the git object bytes onto the ipfs DAG.
        let api = ipfs_api::Shell::new_local().map_err(Error::ApiError)?;
        api.dag_put(&full_obj, "raw", "git").map_err(Error::ApiError)?;
        Ok(full_obj)
    }

    fn enqueue_links(&mut self, obj_bytes: &[u8]) -> Result<(), Error> {
        //let node = ipld_git::parse_object(obj_bytes).map_err(Error::IpldGitError)?;
        let node = match ipld_git::parse_object(obj_bytes) {
            Err(e) => return Err(Error::IpldGitError(e)),
            Ok(node) => node,
        };

        for link in node.links() {
            let link_multihash = multihash::decode(&link.cid.hash)?;
            debug!("        link digest: {:?}", link_multihash.digest);
            self.queue.push_back(git2::Oid::from_bytes(link_multihash.digest)?)
        }
        Ok(())
    }
}
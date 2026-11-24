use crate::{error, files, safety, Result};
use std::{collections::HashSet, io::Read, path::{Path,PathBuf}};
pub const LIMIT:u64=32*1024*1024*1024;
pub struct Stage(pub PathBuf);
impl Stage {pub fn new(parent:&Path)->Result<Self>{safety::reject_links(parent)?;std::fs::create_dir_all(parent).map_err(error)?;let path=parent.join(format!("stage-{}",uuid::Uuid::new_v4()));std::fs::create_dir(&path).map_err(error)?;Ok(Self(path))}}
impl Drop for Stage {fn drop(&mut self){let _=std::fs::remove_dir_all(&self.0);}}
pub fn relative(value:&str)->Result<()> {
    if value.is_empty()||value.len()>1024||value.split('/').count()>32{return Err("Invalid archive path or depth".into());}
    for part in value.split('/') {files::name(part)?;} Ok(())
}
pub fn entries(zip:&mut zip::ZipArchive<std::fs::File>)->Result<Vec<(String,u64)>> {
    if zip.len()>100000{return Err("Archive exceeds 100000 entries".into());}
    let mut names=HashSet::new();let mut total=0_u64;let mut result=Vec::new();
    for i in 0..zip.len(){let item=zip.by_index(i).map_err(error)?;let name=item.name().trim_end_matches('/');relative(name)?;
        if item.unix_mode().is_some_and(|v|v&0o170000==0o120000)||!names.insert(name.to_lowercase()){return Err("Archive links or case-colliding paths are forbidden".into());}
        total=total.checked_add(item.size()).ok_or("Archive size overflow")?;if total>LIMIT{return Err("Expanded archive exceeds 32 GiB".into());}
        result.push((name.to_string(),item.size()));
    } Ok(result)
}
pub fn open(path:&Path)->Result<zip::ZipArchive<std::fs::File>>{safety::reject_links(path)?;let file=std::fs::File::open(path).map_err(error)?;if file.metadata().map_err(error)?.len()>LIMIT{return Err("Archive exceeds 32 GiB".into());}zip::ZipArchive::new(file).map_err(error)}
pub fn extract(path:&Path,destination:&Path)->Result<()> {
    let mut zip=open(path)?;entries(&mut zip)?;
    for i in 0..zip.len(){let mut item=zip.by_index(i).map_err(error)?;let target=destination.join(item.name().trim_end_matches('/'));safety::reject_links(&target)?;
        if item.is_dir(){std::fs::create_dir_all(target).map_err(error)?;continue;}
        std::fs::create_dir_all(target.parent().ok_or("Missing parent")?).map_err(error)?;
        let mut file=std::fs::OpenOptions::new().write(true).create_new(true).open(target).map_err(error)?;let expected=item.size();
        if std::io::copy(&mut (&mut item).take(expected.saturating_add(1)),&mut file).map_err(error)?!=expected{return Err("Archive length mismatch".into());}file.sync_all().map_err(error)?;
    } Ok(())
}
pub fn tree(root:&Path)->Result<Vec<String>> {
    let mut pending=vec![root.to_path_buf()];let mut files=Vec::new();let mut total=0_u64;
    while let Some(dir)=pending.pop(){for entry in std::fs::read_dir(dir).map_err(error)?{let path=entry.map_err(error)?.path();safety::reject_links(&path)?;let name=path.strip_prefix(root).map_err(error)?.to_string_lossy().replace('\\',"/");relative(&name)?;
        if path.is_dir(){pending.push(path);}else if path.is_file(){total=total.checked_add(std::fs::metadata(&path).map_err(error)?.len()).ok_or("Size overflow")?;files.push(name);}
        if total>LIMIT||files.len()+pending.len()>100000{return Err("Tree exceeds archive limits".into());}
    }}Ok(files)
}
pub fn overlay(root:&Path,stage:&Path,names:&[String])->Result<()> {
    let backup=Stage::new(&root.join(".minehost/transactions"))?;let mut saved=Vec::new();let mut created=Vec::new();
    let result=(||{for name in names {relative(name)?;let target=root.join(name);safety::reject_links(&target)?;let source=stage.join(name);safety::reject_links(&source)?;
        std::fs::create_dir_all(target.parent().ok_or("Missing parent")?).map_err(error)?;
        if target.exists(){if !target.is_file(){return Err("Destination is not a regular file".into());}let retained=backup.0.join(name);std::fs::create_dir_all(retained.parent().ok_or("Missing parent")?).map_err(error)?;std::fs::rename(&target,&retained).map_err(error)?;saved.push((target.clone(),retained));}
        std::fs::rename(source,&target).map_err(error)?;created.push(target);
    }Ok(())})();
    if let Err(e)=result{let mut failed=false;for path in created.iter().rev(){if std::fs::remove_file(path).is_err(){failed=true;}}for (path,retained) in saved.iter().rev(){if std::fs::rename(retained,path).is_err(){failed=true;}}
        if failed{let retained=backup.0.clone();std::mem::forget(backup);return Err(format!("{e}; rollback incomplete, recovery data: {}",retained.display()));}return Err(e);
    }
    // Retain replaced files for manual recovery after a successful transaction.
    if !saved.is_empty(){let result=(||{let retained=root.join(".minehost/import-backups");safety::reject_links(&retained)?;std::fs::create_dir_all(&retained).map_err(error)?;std::fs::rename(&backup.0,retained.join(uuid::Uuid::new_v4().to_string())).map_err(error)})();if let Err(e)=result{let path=backup.0.clone();std::mem::forget(backup);return Err(format!("Files installed; backup retention failed: {e}; original files retained at {}",path.display()));}}Ok(())
}
#[cfg(test)]mod tests{use super::*;#[test]fn overlay_rolls_back_all_prior_files(){let root=Stage::new(&std::env::temp_dir()).unwrap();let stage=Stage::new(&std::env::temp_dir()).unwrap();std::fs::write(root.0.join("a.txt"),b"old").unwrap();std::fs::write(stage.0.join("a.txt"),b"new").unwrap();assert!(overlay(&root.0,&stage.0,&["a.txt".into(),"missing.txt".into()]).is_err());assert_eq!(std::fs::read(root.0.join("a.txt")).unwrap(),b"old");}}

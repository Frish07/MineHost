use crate::{archive,error,runtime,safety,Result};
use base64::Engine;
use std::{io::Read,path::Path,process::{Child,Command,Stdio},time::{Duration,Instant},os::windows::io::{AsRawHandle,FromRawHandle,OwnedHandle}};
use windows_sys::Win32::{Foundation::*,Security::{*,Authorization::*,Credentials::*},System::{JobObjects::*,Pipes::*}};
fn wide(value:&str)->Vec<u16>{value.encode_utf16().chain(Some(0)).collect()}
pub fn protect_directory(path:&Path)->Result<()> {
    safety::reject_links(path)?;std::fs::create_dir_all(path).map_err(error)?;
    let sddl=wide("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;OW)");let mut descriptor=std::ptr::null_mut();
    // The descriptor is allocated by Windows and copied by SetFileSecurityW.
    unsafe{if ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(),1,&mut descriptor,std::ptr::null_mut())==0{return Err(error(std::io::Error::last_os_error()));}let path=path.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<_>>();let success=SetFileSecurityW(path.as_ptr(),DACL_SECURITY_INFORMATION|PROTECTED_DACL_SECURITY_INFORMATION,descriptor);let failure=std::io::Error::last_os_error();LocalFree(descriptor);if success==0{return Err(error(failure));}}
    Ok(())
}
pub fn credential_read(target:&str)->Result<Option<String>> {
    let target=wide(target);let mut pointer=std::ptr::null_mut();
    unsafe{if CredReadW(target.as_ptr(),CRED_TYPE_GENERIC,0,&mut pointer)==0{let code=GetLastError();if code==ERROR_NOT_FOUND{return Ok(None);}return Err(format!("Windows credential read failed: {code}"));}
        let credential=&*pointer;let result=if credential.CredentialBlobSize==0{Ok(Some(String::new()))}else if credential.CredentialBlobSize>4096||credential.CredentialBlob.is_null(){Err("Invalid credential data".into())}else{let bytes=std::slice::from_raw_parts(credential.CredentialBlob,credential.CredentialBlobSize as usize);String::from_utf8(bytes.to_vec()).map(Some).map_err(error)};CredFree(pointer.cast());result
    }
}
pub fn credential_write(target:&str,secret:&str)->Result<()> {
    if secret.is_empty()||secret.len()>1024{return Err("Invalid agent credential length".into());}
    let mut target=wide(target);let mut user=wide("MineHost");let mut bytes=secret.as_bytes().to_vec();
    unsafe{let mut credential:CREDENTIALW=std::mem::zeroed();credential.Type=CRED_TYPE_GENERIC;credential.TargetName=target.as_mut_ptr();credential.UserName=user.as_mut_ptr();credential.CredentialBlob=bytes.as_mut_ptr();credential.CredentialBlobSize=bytes.len() as u32;credential.Persist=CRED_PERSIST_LOCAL_MACHINE;let success=CredWriteW(&credential,0);let code=GetLastError();bytes.fill(0);if success==0{return Err(format!("Windows credential write failed: {code}"));}}Ok(())
}
pub fn credential_delete(target:&str)->Result<()> {unsafe{if CredDeleteW(wide(target).as_ptr(),CRED_TYPE_GENERIC,0)==0&&GetLastError()!=ERROR_NOT_FOUND{return Err(error(std::io::Error::last_os_error()));}}Ok(())}
pub fn kill_on_close_job(child:&Child)->Result<OwnedHandle>{
    unsafe{let handle=CreateJobObjectW(std::ptr::null(),std::ptr::null());if handle.is_null(){return Err(error(std::io::Error::last_os_error()));}let job=OwnedHandle::from_raw_handle(handle);let mut limits:JOBOBJECT_EXTENDED_LIMIT_INFORMATION=std::mem::zeroed();limits.BasicLimitInformation.LimitFlags=JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(job.as_raw_handle(),JobObjectExtendedLimitInformation,(&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),std::mem::size_of_val(&limits) as u32)==0||AssignProcessToJobObject(job.as_raw_handle(),child.as_raw_handle())==0{return Err(error(std::io::Error::last_os_error()));}Ok(job)
    }
}
pub fn verify_pipe_owner(handle:std::os::windows::io::RawHandle,pid:u32)->Result<()>{let mut actual=0;unsafe{if GetNamedPipeServerProcessId(handle,&mut actual)==0{return Err(error(std::io::Error::last_os_error()));}}if actual!=pid{return Err("IPC endpoint does not belong to the managed agent".into());}Ok(())}
fn encoded(script:&str)->String{base64::engine::general_purpose::STANDARD.encode(script.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>())}
pub fn powershell(root:&Path,script:&str,input:&serde_json::Value,elevated:bool)->Result<serde_json::Value>{
    let data=base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(input).map_err(error)?);
    let code=format!("$ErrorActionPreference='Stop';[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false);$r=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{data}'))|ConvertFrom-Json;try{{{script}}}catch{{[Console]::Error.WriteLine($_.Exception.Message);exit 1}}");
    let code=if elevated{format!("$ErrorActionPreference='Stop';try{{$p=Start-Process -FilePath (Join-Path $PSHOME 'powershell.exe') -ArgumentList @('-NoProfile','-NonInteractive','-EncodedCommand','{}') -Verb RunAs -WindowStyle Hidden -Wait -PassThru;exit $p.ExitCode}}catch{{[Console]::Error.WriteLine($_.Exception.Message);exit 1}}",encoded(&code))}else{code};
    let stage=archive::Stage::new(&root.join("cache/network-commands"))?;let out=stage.0.join("out.txt");let err=stage.0.join("err.txt");let system=std::env::var_os("SystemRoot").ok_or("Windows system directory is unavailable")?;
    let mut command=Command::new(Path::new(&system).join("System32/WindowsPowerShell/v1.0/powershell.exe"));runtime::hide_window(&mut command);command.args(["-NoProfile","-NonInteractive","-EncodedCommand",&encoded(&code)]).stdin(Stdio::null()).stdout(Stdio::from(std::fs::File::create(&out).map_err(error)?)).stderr(Stdio::from(std::fs::File::create(&err).map_err(error)?));
    let mut child=command.spawn().map_err(error)?;let deadline=Instant::now()+Duration::from_secs(if elevated{120}else{35});let status=loop{if let Some(status)=child.try_wait().map_err(error)?{break status;}if Instant::now()>deadline{let _=child.kill();let _=child.wait();return Err("Windows network command timed out; verify its state before retrying".into());}std::thread::sleep(Duration::from_millis(100));};drop(command);
    let read=|path:&Path|->Result<String>{let file=std::fs::File::open(path).map_err(error)?;let mut bytes=Vec::new();file.take(2*1024*1024+1).read_to_end(&mut bytes).map_err(error)?;if bytes.len()>2*1024*1024{return Err("Network command output exceeds 2 MiB".into());}Ok(String::from_utf8_lossy(&bytes).trim_start_matches('\u{feff}').trim().to_string())};
    if !status.success(){return Err(format!("Windows command failed ({status}): {}",read(&err)?));}let text=read(&out)?;if text.is_empty(){Ok(serde_json::Value::Null)}else{serde_json::from_str(&text).map_err(error)}
}
use std::os::windows::ffi::OsStrExt;
#[cfg(test)]mod tests{use super::*;#[test]fn credential_roundtrip_and_protected_folder(){let target=format!("MineHost/test/{}",uuid::Uuid::new_v4());assert!(credential_read(&target).unwrap().is_none());credential_write(&target,"test-not-a-real-key").unwrap();assert_eq!(credential_read(&target).unwrap().as_deref(),Some("test-not-a-real-key"));credential_delete(&target).unwrap();assert!(credential_read(&target).unwrap().is_none());let stage=archive::Stage::new(&std::env::temp_dir()).unwrap();protect_directory(&stage.0).unwrap();std::fs::write(stage.0.join("test.txt"),b"private").unwrap();}}

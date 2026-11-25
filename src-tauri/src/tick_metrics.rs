use crate::{error, lock, Result, Running, Shared};
use serde::Serialize;
use std::{io::Write, time::{Duration, Instant}};
use tauri::State;
pub struct Query {token: uuid::Uuid, stage: u8, tps: Option<Vec<f64>>, mspt: Option<Vec<f64>>, done: bool}
#[derive(Serialize)] #[serde(rename_all="camelCase")]
pub struct TickMetrics {tps: Option<Vec<f64>>,mspt: Option<Vec<f64>>,sampled_at: u64,source: &'static str}
fn plain(line: &str)->String {
    let mut result=String::new();let mut chars=line.chars();
    while let Some(c)=chars.next() {if c=='\x1b' {if chars.next()==Some('[') {for c in chars.by_ref() {if c.is_ascii_alphabetic(){break;}}}}else if c=='§' {chars.next();}else{result.push(c);}}
    result
}
fn payload(text: &str)->&str {
    let mut text=text.split_once("]: ").map(|(_,v)|v).unwrap_or(text).trim();
    while text.starts_with('[') {if let Some((_,tail))=text.split_once(']'){text=tail.trim();}else{break;}}
    text
}
fn values(text: &str,separator: char,count: usize,max: f64)->Option<Vec<f64>> {
    let values=text.split(separator).map(|s|s.trim().trim_start_matches('*').parse::<f64>().ok()).collect::<Option<Vec<_>>>()?;
    (values.len()==count&&values.iter().all(|v|v.is_finite()&&*v>=0.0&&*v<=max)).then_some(values)
}
impl Query {
    fn line(&mut self,line: &str) {
        let text=plain(line);let text=payload(&text);
        if let Some((_,rest))=text.split_once("TPS from last 5s, 10s, 1m, 5m, 15m:") {self.stage=1;if !rest.trim().is_empty(){self.line(rest);}return;}
        if let Some((_,rest))=text.split_once("Tick durations (min/med/95%ile/max ms) from last 10s, 1m:") {self.stage=2;if !rest.trim().is_empty(){self.line(rest);}return;}
        if text.contains("CPU usage from last 10s, 1m, 15m:") {self.done=self.tps.is_some();self.stage=0;return;}
        if self.stage==1 {if let Some(v)=values(text,',',5,10000.0){self.tps=Some(v);self.stage=0;}}
        else if self.stage==2 {let first=text.split(';').next().unwrap_or("");if let Some(v)=values(first,'/',4,1_000_000.0){self.mspt=Some(v);self.stage=0;}}
        if self.tps.is_some()&&self.mspt.is_some(){self.done=true;}
    }
}
pub fn observe(running: &mut Running,line: &str){if let Some(query)=&mut running.tick_query {query.line(line);}}
struct Guard {shared: Shared,id: String,session: String,token: uuid::Uuid}
impl Drop for Guard {fn drop(&mut self){if let Ok(mut inner)=lock(&self.shared){if let Some(r)=inner.running.get_mut(&self.id){if r.session==self.session&&r.tick_query.as_ref().is_some_and(|q|q.token==self.token){r.tick_query=None;}}}}}
#[tauri::command] pub async fn query_tick_metrics(state: State<'_,Shared>,id: String)->Result<TickMetrics> {
    let shared=state.inner().clone();tauri::async_runtime::spawn_blocking(move ||{
        let token=uuid::Uuid::new_v4();
        let (input,session)={let mut inner=lock(&shared)?;let r=inner.running.get_mut(&id).ok_or("Server is not running")?;
            if r.intent!=crate::lifecycle::Intent::None{return Err("Wait until the server finishes stopping/restarting".into());}
            if r.tick_query.is_some()||r.last_tick_query.is_some_and(|at|at.elapsed()<Duration::from_secs(10)){return Err("Wait at least 10 seconds between tick queries".into());}
            r.last_tick_query=Some(Instant::now());r.tick_query=Some(Query{token,stage:0,tps:None,mspt:None,done:false});(r.input.clone(),r.session.clone())};
        let _guard=Guard{shared:shared.clone(),id:id.clone(),session:session.clone(),token};
        {let mut input=input.lock().map_err(error)?;writeln!(input,"spark tps").map_err(error)?;input.flush().map_err(error)?;}
        let deadline=Instant::now()+Duration::from_secs(10);
        loop {
            {let mut inner=lock(&shared)?;let r=inner.running.get_mut(&id).ok_or("Server stopped while querying ticks")?;
                if r.session!=session{return Err("Server session changed".into());}
                let q=r.tick_query.as_mut().ok_or("Tick query cancelled")?;
                if q.done||Instant::now()>=deadline {if q.tps.is_none()&&q.mspt.is_none(){return Err("No supported spark response within 10s. Check the console: spark may be unavailable or its output format changed. No metrics were inferred.".into());}return Ok(TickMetrics{tps:q.tps.take(),mspt:q.mspt.take(),sampled_at:crate::backup_schedule::now(),source:"spark tps"});}
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }).await.map_err(error)?
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn real_spark_layout_keeps_tps_and_duration_windows_distinct() {
        let mut q=Query{token:uuid::Uuid::new_v4(),stage:0,tps:None,mspt:None,done:false};
        q.line("[12:00:00 INFO]: [⚡] TPS from last 5s, 10s, 1m, 5m, 15m:");q.line("[12:00:00 INFO]: [⚡] 20.0, *20.0, 19.8, 19.9, 19.7");
        q.line("[12:00:00 INFO]: [⚡] Tick durations (min/med/95%ile/max ms) from last 10s, 1m:");q.line("[12:00:00 INFO]: [⚡] 1.0/3.5/12.0/60.0; 1.0/4.0/16.0/100.0");
        assert!(q.done);assert_eq!(q.tps.unwrap()[2],19.8);assert_eq!(q.mspt.unwrap()[1],3.5);
        assert!(values("NaN, 20, 20, 20, 20",',',5,10000.0).is_none());assert!(values("CPU usage 10, 20, 30, 40, 50",',',5,10000.0).is_none());
    }
}

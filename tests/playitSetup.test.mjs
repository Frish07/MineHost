import test from 'node:test';
import assert from 'node:assert/strict';
import {setupPlayit} from '../src/playitSetup.ts';

function fixture(initial={}){
 const calls=[];const phases=[];let cancelled=false;let status={installed:false,linked:false,running:false,phase:'stopped',tunnels:[],...initial};let approval=0;let allocating=0;
 const options={id:'profile',stage:s=>phases.push(s),claim:()=>{},cancelled:()=>cancelled,pause:async()=>{},call:async(command,args)=>{
  calls.push({command,args});
  if(command==='playit_status')return {...status};
  if(command==='playit_install'){status.installed=true;return;}
  if(command==='playit_claim')return 'https://playit.gg/claim/test';
  if(command==='network_open_link')return;
  if(command==='playit_finish_claim'){if(++approval<3)return 'waiting';status.linked=true;return 'linked';}
  if(command==='playit_start'){status.running=true;status.phase='running';return;}
  if(command==='playit_auto_tunnel'){if(++allocating===1)return {phase:'pending',tunnelId:'tunnel',address:null};status.tunnels=[{id:'tunnel',address:'server.ply.gg:1234',disabled:false,matchesProfile:true}];return {phase:'ready',tunnelId:'tunnel',address:'server.ply.gg:1234'};}
  if(command==='network_settings')return {mode:'lan',domain:'keep.example',publicPort:25565};
  if(command==='save_network_settings')return;
  if(command==='playit_stop'){status.running=false;return;}
  throw Error(command);
 }};
 return {options,calls,phases,cancel:()=>{cancelled=true;}};
}
test('one setup waits for browser approval and allocation, then saves the exact reported tunnel',async()=>{
 const f=fixture();assert.equal(await setupPlayit(f.options),'server.ply.gg:1234');
 assert.equal(f.calls.filter(c=>c.command==='network_open_link').length,1);
 assert.equal(f.calls.filter(c=>c.command==='playit_finish_claim').length,3);
 assert.deepEqual(f.calls.find(c=>c.command==='save_network_settings').args.settings,{mode:'playit',domain:'keep.example',publicPort:25565,tunnelId:'tunnel'});
 assert.equal(f.phases.at(-1),'ready');
});
test('an existing installation, account and running agent are reused without restarting or relinking',async()=>{
 const f=fixture({installed:true,linked:true,running:true,phase:'running'});await setupPlayit(f.options);
 for(const command of ['playit_install','playit_claim','playit_start','playit_stop'])assert.equal(f.calls.some(c=>c.command===command),false);
});
test('cancellation during browser approval never starts an agent or creates a tunnel',async()=>{
 const f=fixture();f.options.claim=()=>f.cancel();await assert.rejects(setupPlayit(f.options),/cancelled/);
 assert.equal(f.calls.some(c=>c.command==='playit_start'||c.command==='playit_auto_tunnel'),false);
});
test('cancellation after a creation step stops only an agent started by this setup',async()=>{
 for(const running of [true,false]){
  const f=fixture({installed:true,linked:true,running,phase:running?'running':'stopped'});const call=f.options.call;
  f.options.call=async(command,args)=>{const value=await call(command,args);if(command==='playit_auto_tunnel')f.cancel();return value;};
  await assert.rejects(setupPlayit(f.options),/cancelled/);
  assert.equal(f.calls.filter(c=>c.command==='playit_stop').length,running?0:1);
  assert.equal(f.calls.some(c=>c.command==='save_network_settings'),false);
 }
});
test('an unreported or wrong-profile tunnel cannot become the saved player endpoint',async()=>{
 const f=fixture({installed:true,linked:true,running:true,phase:'running'});const call=f.options.call;
 f.options.call=async(command,args)=>{const value=await call(command,args);if(command==='playit_status')return {...value,tunnels:[{id:'other',disabled:false,matchesProfile:true,address:'wrong.example:1'}]};return value;};
 await assert.rejects(setupPlayit(f.options),/not yet reported/);
 assert.equal(f.calls.some(c=>c.command==='save_network_settings'),false);
});

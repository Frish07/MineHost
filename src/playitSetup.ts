export type SetupStatus={installed:boolean;linked:boolean;running:boolean;phase:string;tunnels:{id:string;address:string;disabled:boolean;matchesProfile:boolean}[]};
type AutoTunnel={phase:string;tunnelId:string|null;address:string|null};
export type SetupStage='checking'|'downloading'|'browser'|'starting'|'creating'|'waiting'|'ready';
type Options={
 id:string;
 call:<T>(command:string,args?:Record<string,unknown>)=>Promise<T>;
 stage:(value:SetupStage)=>void;
 claim:(url:string)=>void;
 cancelled:()=>boolean;
 pause:()=>Promise<void>;
};
export async function setupPlayit(o:Options):Promise<string>{
 let started=false;
 const check=()=>{if(o.cancelled())throw new Error('Setup cancelled / Настройка отменена');};
 const status=()=>o.call<SetupStatus>('playit_status',{id:o.id});
 try{
  check();o.stage('checking');let current=await status();check();
  if(!current.installed){o.stage('downloading');await o.call('playit_install',{consent:true});check();}
  if(!current.linked){
   o.stage('browser');const url=await o.call<string>('playit_claim',{consent:true});check();o.claim(url);await o.call('network_open_link',{kind:'playit-claim',claimUrl:url});
   let linked=false;
   for(let attempt=0;attempt<200;attempt++){check();const state=await o.call<string>('playit_finish_claim');check();if(state==='linked'){linked=true;break;}if(state==='rejected')throw new Error('Playit link rejected / Привязка отклонена');await o.pause();}
   if(!linked)throw new Error('Browser approval timed out / Время подтверждения в браузере истекло');
   o.claim('');
  }
  check();current=await status();check();
  if(!current.running){o.stage('starting');await o.call('playit_start',{consent:true});started=true;check();}
  let online=false;
  for(let attempt=0;attempt<40;attempt++){check();current=await status();if(current.phase==='running'){online=true;break;}if(['has_invalid_secret','disabled_over_limit','error'].includes(current.phase))throw new Error(`Playit: ${current.phase}`);await o.pause();}
  check();if(!online)throw new Error('Agent connection timed out / Агент не подключился за отведённое время');
  o.stage('creating');let result:AutoTunnel|null=null;
  for(let attempt=0;attempt<60;attempt++){check();result=await o.call<AutoTunnel>('playit_auto_tunnel',{id:o.id,consent:true});check();if(result.phase==='ready')break;o.stage('waiting');await o.pause();}
  if(!result?.tunnelId||!result.address||result.phase!=='ready')throw new Error('Playit is still allocating the tunnel. Check the dashboard; setup will not create duplicates. / Playit ещё настраивает туннель. Проверьте кабинет; повторная настройка не создаёт дубликаты.');
  // Wait until this exact managed agent reports the allocated tunnel, then save it.
  let reported=false;
  for(let attempt=0;attempt<40;attempt++){check();current=await status();if(current.tunnels.some(t=>t.id===result?.tunnelId&&t.matchesProfile&&!t.disabled)){reported=true;break;}await o.pause();}
  check();if(!reported)throw new Error('Tunnel created but not yet reported by the agent. Retry setup. / Туннель создан, но агент ещё не сообщил о нём. Повторите настройку.');
  const settings=await o.call<Record<string,unknown>>('network_settings',{id:o.id});check();await o.call('save_network_settings',{id:o.id,settings:{...settings,mode:'playit',tunnelId:result.tunnelId}});check();o.stage('ready');return result.address;
 }finally{
  // Cancellation closes only an agent started by this setup; remote tunnels remain.
  if(o.cancelled()&&started)await o.call('playit_stop',{consent:true});
 }
}

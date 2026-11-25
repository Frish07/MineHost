// Runs only on the GitHub Windows runner. All Tauri calls use an in-memory fixture.
import {chromium,expect} from '@playwright/test';
import {spawn} from 'node:child_process';
import {mkdir} from 'node:fs/promises';
const preview=spawn(process.execPath,['node_modules/vite/bin/vite.js','preview','--host','127.0.0.1','--port','4173','--strictPort'],{stdio:'ignore',windowsHide:true});
let browser;
try{
 for(let i=0;i<60;i++){try{if((await fetch('http://127.0.0.1:4173')).ok)break;}catch{}await new Promise(r=>setTimeout(r,500));}
 await mkdir('ui-results',{recursive:true});browser=await chromium.launch();const page=await browser.newPage({viewport:{width:1440,height:1000}});const errors=[];page.on('pageerror',e=>errors.push(e.message));page.on('dialog',d=>d.accept());
 await page.addInitScript(()=>{
  const server=(id,name,software,port)=>({id,name,software,port,version:'1.21.11',ramMb:2048,eula:true,installed:true,javaMajor:21,running:false,busy:false});
  let servers=[server('paper','Paper QA','paper',25565),server('fabric','Fabric QA','fabric',25566),server('vanilla','Vanilla QA','vanilla',25567)];let running=false;const worlds=[];const calls=[];window.__qa={calls};let callback=1;
  window.__TAURI_INTERNALS__={metadata:{currentWindow:{label:'main'},currentWebview:{label:'main'}},transformCallback:()=>callback++,unregisterCallback:()=>{},invoke:async(command,args={})=>{
   calls.push({command,args});
   if(command==='list_servers')return structuredClone(servers);
   if(command==='server_logs')return [];
   if(command.includes('listen'))return callback++;
   if(command==='list_addons')return [{fileName:'sample.jar',enabled:true,size:1234,provider:null,projectId:null,versionId:null,versionNumber:'1.0'}];
   if(command==='addon_details')return {fileName:'sample.jar',descriptor:'plugin.yml',content:'name: Sample\nversion: 1.0\nmain: sample.Main'};
   if(command==='plugin:dialog|open'){if(args.options.directory)return 'C:\\Worlds\\folder_world';if(args.options.multiple)return ['C:\\Downloads\\sample.jar'];return 'C:\\Worlds\\zip_world.zip';}
   if(command==='import_addon')return;
   if(command==='list_worlds')return structuredClone(worlds);
   if(command==='import_world'){worlds.push({name:args.name,active:false,generated:true,nether:false,end:false});return;}
   if(command==='playit_status')return {installed:true,version:'2.0.2',linked:true,running,phase:running?'running':'stopped',noticeCount:0,pendingTunnels:0,tunnels:running?[{id:'tunnel',address:'qa.ply.gg:1234',destination:'127.0.0.1:25565',disabled:false,matchesProfile:true}]:[]};
   if(command==='playit_start'){running=true;return;}
   if(command==='playit_stop'){running=false;return;}
   if(command==='delete_server'){servers=servers.filter(s=>s.id!==args.id);return;}
   return null;
  }};
 });
 await page.goto('http://127.0.0.1:4173');const nav=page.locator('aside nav');
 await expect(page.getByRole('button',{name:'Paper QA',exact:true})).toBeVisible();await expect(nav.getByRole('button',{name:'Плагины',exact:true})).toHaveCount(0);await expect(nav.getByRole('button',{name:'Моды',exact:true})).toHaveCount(0);await page.screenshot({path:'ui-results/01-servers.png',fullPage:true});
 await page.getByRole('button',{name:'Paper QA',exact:true}).click();await expect(nav.getByRole('button',{name:'Плагины',exact:true})).toBeVisible();await expect(nav.getByRole('button',{name:'Моды',exact:true})).toHaveCount(0);await expect(page.locator('h1')).toHaveText('Paper QA');await page.screenshot({path:'ui-results/02-server.png',fullPage:true});
 await nav.getByRole('button',{name:'Плагины',exact:true}).click();await page.locator('.addons-panel input[type=checkbox]').check();await page.getByRole('button',{name:'Загрузить плагины с компьютера'}).click();await expect.poll(()=>page.evaluate(()=>window.__qa.calls.filter(c=>c.command==='import_addon').length)).toBe(1);await page.getByRole('button',{name:'Подробнее',exact:true}).click();await expect(page.locator('.addon-description')).toContainText('name: Sample');await page.screenshot({path:'ui-results/03-plugins.png',fullPage:true});
 await nav.getByRole('button',{name:'Миры',exact:true}).click();await page.getByRole('button',{name:'↑ Загрузить свой мир'}).click();let dialog=page.getByRole('dialog');await dialog.getByRole('button',{name:'Выбрать папку'}).click();await expect(dialog.getByRole('textbox')).toHaveValue('folder_world');await page.screenshot({path:'ui-results/04-world-upload.png',fullPage:true});await dialog.getByRole('button',{name:'Загрузить мир',exact:true}).click();await expect(dialog).toHaveCount(0);
 await page.getByRole('button',{name:'↑ Загрузить свой мир'}).click();dialog=page.getByRole('dialog');await dialog.getByRole('button',{name:'Выбрать ZIP'}).click();await dialog.getByRole('button',{name:'Загрузить мир',exact:true}).click();await expect(dialog).toHaveCount(0);await expect.poll(()=>page.evaluate(()=>window.__qa.calls.filter(c=>c.command==='import_world').length)).toBe(2);
 await nav.getByRole('button',{name:'playit.gg',exact:true}).click();await page.getByRole('button',{name:'Открыть доступ playit',exact:true}).click();await expect(page.getByRole('button',{name:'Отключить интернет-доступ playit'})).toBeVisible();await page.screenshot({path:'ui-results/05-playit.png',fullPage:true});await page.getByRole('button',{name:'Отключить интернет-доступ playit'}).click();await expect(page.getByRole('button',{name:'Открыть доступ playit',exact:true})).toBeVisible();
 await nav.getByRole('button',{name:'Удалить сервер',exact:true}).click();await page.locator('main').getByRole('button',{name:'Удалить сервер',exact:true}).click();dialog=page.getByRole('dialog');await expect(dialog.getByRole('button',{name:'Удалить навсегда'})).toBeDisabled();await dialog.getByRole('textbox').fill('wrong');await dialog.getByRole('checkbox').check();await expect(dialog.getByRole('button',{name:'Удалить навсегда'})).toBeDisabled();await page.screenshot({path:'ui-results/06-delete-confirmation.png',fullPage:true});await dialog.getByRole('button',{name:'Отмена',exact:true}).click();await expect.poll(()=>page.evaluate(()=>window.__qa.calls.filter(c=>c.command==='delete_server').length)).toBe(0);
 await page.locator('main').getByRole('button',{name:'Удалить сервер',exact:true}).click();dialog=page.getByRole('dialog');await dialog.getByRole('textbox').fill('Paper QA');await dialog.getByRole('checkbox').check();await dialog.getByRole('button',{name:'Удалить навсегда'}).click();await expect(page.getByRole('button',{name:'Paper QA',exact:true})).toHaveCount(0);await expect(page.locator('h1')).toHaveText('Мои серверы');
 await page.getByRole('button',{name:'Fabric QA',exact:true}).click();await expect(nav.getByRole('button',{name:'Моды',exact:true})).toBeVisible();await expect(nav.getByRole('button',{name:'Плагины',exact:true})).toHaveCount(0);await nav.getByRole('button',{name:'Моды',exact:true}).click();await expect(page.getByRole('button',{name:'Загрузить моды с компьютера'})).toBeVisible();await page.screenshot({path:'ui-results/07-mods.png',fullPage:true});
 await page.setViewportSize({width:740,height:1000});await page.getByRole('button',{name:'Светлая тема'}).click();const contrast=await page.getByRole('button',{name:'Каталог Modrinth',exact:true}).evaluate(button=>{const s=getComputedStyle(button);const l=value=>{const rgb=value.match(/[\d.]+/g).slice(0,3).map(Number).map(c=>{c/=255;return c<=0.04045?c/12.92:((c+0.055)/1.055)**2.4;});return rgb[0]*0.2126+rgb[1]*0.7152+rgb[2]*0.0722;};const a=l(s.color),b=l(s.backgroundColor);return (Math.max(a,b)+0.05)/(Math.min(a,b)+0.05);});expect(contrast).toBeGreaterThanOrEqual(4.5);await page.screenshot({path:'ui-results/08-narrow-light.png',fullPage:true});await nav.getByRole('button',{name:'Мои серверы',exact:true}).click();await page.getByRole('button',{name:'Vanilla QA',exact:true}).click();await expect(nav.getByRole('button',{name:'Моды',exact:true})).toHaveCount(0);await expect(nav.getByRole('button',{name:'Плагины',exact:true})).toHaveCount(0);
 expect(errors).toEqual([]);console.log('UI smoke passed: server navigation, core capabilities, JAR import/details, folder/ZIP selection, playit start/stop and deletion confirmation.');
}finally{await browser?.close();preview.kill();}

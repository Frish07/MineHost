import React, { useCallback, useEffect, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { Channel, invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow';
import './style.css';
import { type DownloadProgress } from './download';
import { DownloadStatus as PaperDownloadStatus } from './DownloadStatus';
import { AddonsPanel } from './AddonsPanel';
import { FilesPanel } from './FilesPanel';
import { WorldsPanel } from './WorldsPanel';
import { PlayersPanel } from './PlayersPanel';
import {BackupsPanel} from './BackupsPanel';
import {ModpacksPanel} from './ModpacksPanel';
import {NetworkPanel} from './NetworkPanel';
import {MonitoringPanel} from './MonitoringPanel';
type Software = 'paper' | 'vanilla' | 'purpur' | 'fabric' | 'forge' | 'neoforge';
const softwareLabels: Record<Software, string> = {paper:'Paper', vanilla:'Vanilla', purpur:'Purpur', fabric:'Fabric',forge:'Forge',neoforge:'NeoForge'};

type Server = { id: string; name: string; version: string; port: number; ramMb: number; eula: boolean; installed: boolean; javaMajor: number; software: Software; running: boolean; busy: boolean };
type Runtime = {path: string; major: number};
type Page = 'monitoring' | 'overview' | 'console' | 'software' | 'plugins' | 'modpacks' | 'files' | 'worlds' | 'players' | 'properties' | 'backups' | 'network' | 'settings';
type Language = 'ru' | 'en';
const copy = {
  ru: { monitoring: 'Мониторинг', modpacks: 'Модпаки', files: 'Файлы', worlds: 'Миры', players: 'Игроки', overview: 'Мои серверы', console: 'Консоль', software: 'Сервер и Java', plugins: 'Моды и плагины', properties: 'Настройки сервера', backups: 'Резервные копии', network: 'Подключение', settings: 'Настройки MineHost', create: 'Создать сервер', start: 'Запустить', stop: 'Остановить', folder: 'Открыть папку', online: 'Процесс запущен', offline: 'Остановлен', busy: 'Выполняется операция', subtitle: 'Ваш мир. Ваши правила.', empty: 'Здесь начинается ваш новый мир', emptyText: 'Создайте независимый Paper-сервер. Данные остаются на вашем компьютере.', name: 'Название', version: 'Версия Minecraft', ram: 'Память (MiB)', port: 'Порт', cancel: 'Отмена', save: 'Сохранить', refresh: 'Обновить', install: 'Скачать серверное ПО', eula: 'Я прочитал(а) и принимаю Minecraft EULA', downloadJava: 'Скачать Java runtime', checkJava: 'Найти Java', javaPath: 'Путь к java.exe (необязательно)', send: 'Отправить', command: 'Команда сервера, например list', search: 'Поиск по логам', plugin: 'Выбрать JAR', pluginRisk: 'Я понимаю, что плагины выполняют код с моими правами. Устанавливаю только доверенные файлы.', backup: 'Создать ZIP-копию', delete: 'Удалить сервер', closeWarning: 'Сначала остановите серверы, управляемый playit-агент и дождитесь завершения операций.', local: 'Локальный адрес', notice: '0.7.0 · 6 ядер · Modrinth', stoppedOnly: 'Изменения и резервные копии доступны после остановки сервера.', processing: 'Операция выполняется…', done: 'Готово', choose: 'Выберите сервер', light: 'Светлая тема', dark: 'Тёмная тема', confirmedJava: 'Скачать Temurin JRE с Adoptium в приватную папку MineHost? Java нужна только для Minecraft. Rust, Node.js и другие инструменты установлены не будут. Лицензии Java находятся в скачанном runtime.', consoleHint: 'Логи появятся после запуска. Наличие процесса ещё не означает готовность сервера: дождитесь строки Done.', backupsHint: 'Копия создаётся только при остановленном сервере: это сохраняет согласованность мира. Архив содержит все файлы профиля и хранится рядом с каталогом servers, в backups.', networkHint: 'На этом ПК подключайтесь по адресу ниже. Для LAN используйте IPv4 компьютера из настроек Windows. Открытие доступа через интернет требует отдельной настройки маршрутизатора и брандмауэра.', future: 'Туннели playit.gg, DNS и автоматическая настройка сети запланированы. MineHost сейчас не изменяет брандмауэр или маршрутизатор.', typeDelete: 'Уберите ненужные сетевые правила и пробросы в разделе «Подключение». Введите точное имя сервера; папка будет перемещена в trash:', loadVersions: 'Загрузить версии', error: 'Ошибка', propertiesHint: 'Редактор key=value. Перед сохранением создаётся .bak. Сохраните server-port и online-mode=true; экранирование и дополнительные сетевые слушатели пока не поддерживаются.' },
  en: { monitoring: 'Monitoring', modpacks: 'Modpacks', files: 'Files', worlds: 'Worlds', players: 'Players', overview: 'My servers', console: 'Console', software: 'Server & Java', plugins: 'Mods & plugins', properties: 'Server settings', backups: 'Backups', network: 'Connection', settings: 'MineHost settings', create: 'Create server', start: 'Start', stop: 'Stop', folder: 'Open folder', online: 'Process running', offline: 'Stopped', busy: 'Operation in progress', subtitle: 'Your world. Your rules.', empty: 'Your next world starts here', emptyText: 'Create an independent Paper server. Your data stays on your computer.', name: 'Name', version: 'Minecraft version', ram: 'Memory (MiB)', port: 'Port', cancel: 'Cancel', save: 'Save', refresh: 'Refresh', install: 'Download server software', eula: 'I have read and accept the Minecraft EULA', downloadJava: 'Download Java runtime', checkJava: 'Find Java', javaPath: 'Path to java.exe (optional)', send: 'Send', command: 'Server command, e.g. list', search: 'Search logs', plugin: 'Choose JAR', pluginRisk: 'I understand plugins execute code with my user permissions. I only install trusted files.', backup: 'Create ZIP backup', delete: 'Delete server', closeWarning: 'Stop all servers and the managed playit agent, and wait for operations before closing.', local: 'Local address', notice: '0.7.0 · 6 server types · Modrinth', stoppedOnly: 'Changes and backups require a stopped server.', processing: 'Working…', done: 'Done', choose: 'Select a server', light: 'Light theme', dark: 'Dark theme', confirmedJava: 'Download Temurin JRE from Adoptium into a private MineHost folder? Java is only needed for Minecraft. Rust, Node.js and development tools will not be installed. Java licenses are included in the downloaded runtime.', consoleHint: 'Logs appear after launch. A running process does not mean the server is ready: wait for the Done message.', backupsHint: 'Backups require a stopped server to keep world data consistent. The archive includes all profile files and is stored in backups, next to the servers directory.', networkHint: 'On this computer, connect with the address below. For LAN, use this PC’s IPv4 from Windows settings. Internet access requires separate router and firewall configuration.', future: 'playit.gg tunnels, DNS and automatic networking are planned. MineHost currently does not modify your firewall or router.', typeDelete: 'Remove unneeded network rules and mappings in Connection first. Type the exact server name; its folder will move to trash:', loadVersions: 'Load versions', error: 'Error', propertiesHint: 'Strict key=value editor. A .bak is saved before changes. Keep server-port and online-mode=true; escapes and extra network listeners are not supported yet.' }
};
const icons: Record<Page, string> = {overview: '▦', monitoring: '▥', console: '›_', software: '⬡', plugins: '◇', modpacks:'▦', files: '▤', worlds: '▧', players: '♙', properties: '☷', backups: '◷', network: '◎', settings: '⚙'};

function App() {
  const [language, setLanguage] = useState<Language>(() => localStorage.getItem('language') === 'en' ? 'en' : 'ru');
  const t = copy[language];
  const [theme, setTheme] = useState(() => localStorage.getItem('theme') ?? 'dark');
  const [page, setPage] = useState<Page>('overview');
  const [servers, setServers] = useState<Server[]>([]);
  const [selected, setSelected] = useState('');
  const [message, setMessage] = useState('');
  const [failure, setFailure] = useState(false);
  const [working, setWorking] = useState(false);
  const [downloads, setDownloads] = useState<Record<string, DownloadProgress>>({});
  const [creating, setCreating] = useState(false);
  const [versions, setVersions] = useState<string[]>([]);
  const [name, setName] = useState('Survival');
  const [version, setVersion] = useState('');
  const [software, setSoftware] = useState<Software>('paper');
  const [loaderVersion,setLoaderVersion]=useState('');
  const [port, setPort] = useState(25565);
  const [ram, setRam] = useState(2048);
  const [javaPath, setJavaPath] = useState('');
  const [javaInfo, setJavaInfo] = useState('');
  const [logs, setLogs] = useState<string[]>([]);
  const [search, setSearch] = useState('');
  const [logLevel, setLogLevel] = useState('all');
  const [command, setCommand] = useState('');
  const [history, setHistory] = useState<string[]>([]);
  const [historyIndex, setHistoryIndex] = useState(-1);
  const [properties, setProperties] = useState('');
  const [pluginConsent, setPluginConsent] = useState(false);
  const selectedRef = useRef(selected);
  const consentRef = useRef(pluginConsent);
  const logBox = useRef<HTMLPreElement>(null);
  const server = servers.find(s => s.id === selected);
  const refresh = useCallback(async () => {
    const list = await invoke<Server[]>('list_servers');
    setServers(list);
    setSelected(current => list.some(s => s.id === current) ? current : list[0]?.id ?? '');
  }, []);
  const act = useCallback(async (fn: () => Promise<unknown>) => {
    setWorking(true); setMessage(''); setFailure(false);
    try { const result = await fn(); setMessage(typeof result === 'string' ? result : t.done); setFailure(false); }
    catch (e) { setMessage(String(e)); setFailure(true); }
    finally { setWorking(false); await refresh().catch(e => {setMessage(String(e)); setFailure(true);}); }
  }, [refresh, t.done]);
  useEffect(() => { document.documentElement.dataset.theme = theme; localStorage.setItem('theme', theme); }, [theme]);
  useEffect(() => { localStorage.setItem('language', language); document.documentElement.lang = language; }, [language]);
  useEffect(() => { selectedRef.current = selected; setJavaInfo(''); setJavaPath(''); setHistory([]); setHistoryIndex(-1); }, [selected]);
  useEffect(() => { consentRef.current = pluginConsent; }, [pluginConsent]);
  useEffect(() => { void refresh().catch(e => { setFailure(true); setMessage(String(e)); }); }, [refresh]);
  useEffect(() => {
    let disposed = false; const cleanups: (() => void)[] = [];
    const keep = (unlisten: () => void) => { if (disposed) unlisten(); else cleanups.push(unlisten); };
    void listen<{id: string; line: string}>('server-log', event => {
      if (event.payload.id === selectedRef.current) setLogs(lines => [...lines.slice(-1999), event.payload.line]);
    }).then(keep);
    void listen('servers-changed', () => {void refresh();}).then(keep);
    void listen('exit-blocked', () => { setFailure(true); setMessage(t.closeWarning); }).then(keep);
    void getCurrentWebviewWindow().onDragDropEvent(event => {
      if (event.payload.type === 'drop' && selectedRef.current && consentRef.current) {
        const id = selectedRef.current;
        const paths = event.payload.paths;
        void act(async () => { for (const source of paths) await invoke('import_addon', {id, source, consent: true}); });
      }
    }).then(keep);
    return () => { disposed = true; cleanups.forEach(fn => fn()); };
  }, [act, refresh, t.closeWarning]);
  useEffect(() => {
    setLogs([]);
    if (!selected) return;
    let cancelled = false;
    void invoke<string[]>('server_logs', {id: selected}).then(lines => { if (!cancelled) setLogs(lines); }).catch(e => { if (!cancelled) {setMessage(String(e)); setFailure(true);} });
    return () => {cancelled = true;};
  }, [selected]);
  useEffect(() => {
    if (!selected || page !== 'properties') return;
    let cancelled = false;
    void invoke<string>('read_properties', {id: selected}).then(content => { if (!cancelled) setProperties(content); }).catch(e => {if (!cancelled) {setMessage(String(e)); setFailure(true);}});
    return () => {cancelled = true;};
  }, [selected, page]);
  useEffect(() => { if (logBox.current) logBox.current.scrollTop = logBox.current.scrollHeight; }, [logs]);
  const call = (method: string, args: Record<string, unknown> = {}) => act(() => invoke(method, {id: selected, ...args}));
  const downloadPaper = () => {
    const installerConsent=!!server&&['forge','neoforge'].includes(server.software)&&window.confirm(language==='ru'?'Запустить официальный установщик Forge/NeoForge через выбранную Java для подготовки Minecraft-сервера? Это не сборка MineHost. Сначала найдите или скачайте подходящую Java.':'Execute the official Forge/NeoForge installer using the selected Java to prepare a Minecraft server? This does not build MineHost. Detect or download the required Java first.');
    if(server&&['forge','neoforge'].includes(server.software)&&!installerConsent)return;
    const id = selected;
    void act(async () => {
      setDownloads(current => ({...current, [id]: {phase: 'resolving', downloaded: 0, total: null}}));
      const onProgress = new Channel<DownloadProgress>();
      onProgress.onmessage = progress => setDownloads(current => ({...current, [id]: progress}));
      try {
        await invoke('install_software', {id, onProgress, customPath:javaPath, installerConsent});
      } catch (error) {
        setDownloads(current => ({...current, [id]: {...current[id], phase: 'failed', error: String(error)}}));
        throw error;
      }
    });
  };
  const loadVersions = () => act(async () => { const values = await invoke<string[]>('software_versions', {software}); setVersions(values); if (values[0]) setVersion(values[0]); });
  const locked = working || server?.busy === true;
  const cannotEdit = locked || server?.running === true;
  const logPattern = logLevel === 'all' ? null : logLevel === 'minehost' ? /\[MineHost\]/ : new RegExp(`\\b${logLevel}\\b`);
  return <div className="app">
    <aside>
      <div className="brand"><span className="brand-icon">M</span><span>Mine<span className="accent">Host</span><small>MINECRAFT SERVER MANAGER</small></span></div>
      <div className="nav-label">WORKSPACE</div>
      <nav>{(Object.keys(icons) as Page[]).map(key => <button key={key} className={page === key ? 'active' : ''} onClick={() => setPage(key)}><span>{icons[key]}</span>{t[key]}</button>)}</nav>
      <div className="sidebar-bottom"><span className="local-dot"/> LOCAL · WINDOWS x64<p>{t.subtitle}</p></div>
    </aside>
    <main>
      <header><div className="breadcrumb">MineHost <span>/</span> {t[page]}</div><div className="header-actions"><button onClick={() => setLanguage(language === 'ru' ? 'en' : 'ru')}>{language.toUpperCase()}</button><button aria-label={theme === 'dark' ? t.light : t.dark} onClick={() => setTheme(theme === 'dark' ? 'light' : 'dark')}>{theme === 'dark' ? '☀' : '☾'}</button></div></header>
      <div className="content">
        <div className="page-title"><div><p className="eyebrow">YOUR PERSONAL SERVER SPACE</p><h1>{t[page]}</h1><p className="muted">{t.notice}</p></div><button className="primary" disabled={working} onClick={() => {setCreating(true); setPort(servers.reduce((p, s) => Math.max(p, s.port + 1), 25565));}}>＋ {t.create}</button></div>
        {(message || working) && <div role={failure ? 'alert' : 'status'} className={`notice ${failure ? 'error' : ''}`}>{working ? t.processing : message}<button aria-label="Close" onClick={() => setMessage('')}>×</button></div>}
        {page === 'overview' ? <>
          <div className="stats"><div><span>{t.overview}</span><strong>{servers.length.toString().padStart(2, '0')}</strong></div><div><span>{t.online}</span><strong className="accent">{servers.filter(s => s.running).length.toString().padStart(2, '0')}</strong></div><div><span>JAVA HEAP / 12 GiB</span><strong>{(servers.filter(s => s.running).reduce((sum, s) => sum + s.ramMb, 0) / 1024).toFixed(1)} <small>GiB</small></strong></div></div>
          {!servers.length && <div className="empty"><div className="world-icon">▧</div><h2>{t.empty}</h2><p>{t.emptyText}</p><button className="primary" onClick={() => setCreating(true)}>＋ {t.create}</button></div>}
          <div className="server-grid">{servers.map(s => <article className="server-card" key={s.id}>
            <div className="server-banner"><span className="terrain">▰</span><span className={`status ${s.running ? 'online' : ''}`}>● {s.busy ? t.busy : s.running ? t.online : t.offline}</span></div>
            <div className="card-body"><div className="server-name"><h2>{s.name}</h2><span>{softwareLabels[s.software]}</span></div><p className="muted">Minecraft {s.version} · {s.ramMb} MiB</p><div className="address">127.0.0.1:{s.port}<span>JAVA</span></div><div className="card-actions"><button className={s.running ? 'danger' : 'primary'} disabled={working || s.busy} onClick={() => {setSelected(s.id); void act(() => invoke(s.running ? 'stop_server' : 'start_server', {id:s.id, customPath: ''}));}}>{s.running ? '■' : '▶'} {s.running ? t.stop : t.start}</button><button onClick={() => {setSelected(s.id); setPage(s.installed ? 'console' : 'software');}}>→</button></div></div>
          </article>)}</div>
        </> : page === 'settings' ? <section className="panel"><h2>MineHost 0.7.0</h2><p>{t.notice}</p><p>{t.closeWarning}</p><p className="muted">Tauri 2 · Rust · React · SQLite</p><p className="muted">GitHub Actions → Windows x64 → SHA-256</p><button disabled={working} onClick={()=>{if(window.confirm(language === 'ru' ? 'Остановить управляемый playit-агент и все его туннели?' : 'Stop the managed playit agent and all its tunnels?'))void act(()=>invoke('playit_stop',{consent:true}));}}>{language === 'ru' ? 'Остановить playit перед выходом' : 'Stop playit before exiting'}</button></section> : <>
          <div className="server-toolbar"><select aria-label={t.choose} value={selected} onChange={e => setSelected(e.target.value)}><option value="" disabled>{t.choose}</option>{servers.map(s => <option key={s.id} value={s.id}>{s.name} · :{s.port}</option>)}</select><span className={`status ${server?.running ? 'online' : ''}`}>● {server?.running ? t.online : t.offline}</span><button disabled={!server || locked} onClick={() => call('open_folder')}>{t.folder}</button><button disabled={working} onClick={() => act(refresh)}>{t.refresh}</button></div>
          {server && <>
            {page === 'console' && <section className="panel console-panel"><div className="panel-heading"><h2>{server.name}</h2><button className={server.running ? 'danger' : 'primary'} disabled={locked} onClick={() => call(server.running ? 'stop_server' : 'start_server', {customPath: javaPath})}>{server.running ? t.stop : t.start}</button><button disabled={locked || !server.running} onClick={() => call('restart_server')}>{language === 'ru' ? 'Перезапустить' : 'Restart'}</button></div><p className="muted">{t.consoleHint}</p><input placeholder={t.search} value={search} onChange={e => setSearch(e.target.value)}/><select aria-label={language === 'ru' ? 'Уровень логов' : 'Log level'} value={logLevel} onChange={e=>setLogLevel(e.target.value)}>{['all','INFO','WARN','ERROR','FATAL','DEBUG','TRACE','minehost'].map(level=><option key={level} value={level}>{level === 'all' ? (language === 'ru' ? 'Все сообщения' : 'All messages') : level}</option>)}</select><pre ref={logBox} className="logs" aria-live="off">{logs.filter(line => (!logPattern || logPattern.test(line)) && line.toLowerCase().includes(search.toLowerCase())).join('\n')}</pre><form className="command-line" onSubmit={e => {e.preventDefault(); const value = command; void act(async () => {await invoke('send_command', {id: selected, command: value}); setHistory(h => [value, ...h].slice(0,100)); setHistoryIndex(-1); setCommand('');});}}><span>›</span><input placeholder={t.command} value={command} disabled={!server.running || working} onChange={e => setCommand(e.target.value)} onKeyDown={e => {if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {e.preventDefault(); const i = Math.max(-1, Math.min(history.length - 1, historyIndex + (e.key === 'ArrowUp' ? 1 : -1))); setHistoryIndex(i); setCommand(history[i] ?? '');}}}/><button disabled={!server.running || working || !command.trim()}>{t.send}</button></form></section>}
            {page === 'software' && <section className="panel"><div className="panel-heading"><h2>{softwareLabels[server.software]} {server.version}</h2><span className="tag">Java {server.javaMajor}</span></div><p className="muted">{server.software === 'fabric' ? (language === 'ru' ? 'Fabric Meta не публикует checksum launcher: HTTPS и проверка формата JAR. Библиотеки и Minecraft будут загружены Fabric при первом запуске; следите за консолью.' : 'Fabric Meta provides no launcher checksum: HTTPS and JAR format verification. Fabric downloads libraries and Minecraft on first launch; watch the console.') : ({paper:'HTTPS · SHA-256', vanilla:'HTTPS · SHA-1 (Mojang)', purpur:'HTTPS · MD5 (Purpur API)', fabric:'',forge:'Official Maven · SHA-1 · Java installer',neoforge:'Official Maven · SHA-1 · Java installer'}[server.software])}</p><p className="muted">{t.stoppedOnly}</p><button className="primary" disabled={cannotEdit} onClick={downloadPaper}>↓ {t.install}</button>{downloads[server.id] && <PaperDownloadStatus progress={downloads[server.id]} language={language}/>}<div className="divider"/><label className="check"><input type="checkbox" checked={server.eula} disabled={cannotEdit || server.eula || !server.installed} onChange={e => {if(e.target.checked) void call('accept_eula', {consent:true});}}/>{t.eula}</label><p className="muted selectable">https://www.minecraft.net/eula</p><div className="divider"/><h2>Java runtime</h2><p className="muted">Temurin JRE · Windows x64 · SHA-256</p><label>{t.javaPath}<input placeholder="C:\Program Files\...\bin\java.exe" value={javaPath} onChange={e => setJavaPath(e.target.value)}/></label><div className="button-row"><button disabled={locked} onClick={() => act(async () => {const info = await invoke<Runtime>('java_status', {id:selected, customPath:javaPath}); setJavaInfo(`Java ${info.major} · ${info.path}`);})}>{t.checkJava}</button><button disabled={cannotEdit} onClick={() => {if (window.confirm(t.confirmedJava)) void act(async () => {const info = await invoke<Runtime>('install_java', {id: selected, consent:true}); setJavaInfo(`Java ${info.major} · ${info.path}`);});}}>{t.downloadJava}</button><button className="primary" disabled={locked || !server.eula || !server.installed || server.running} onClick={() => call('start_server', {customPath:javaPath})}>▶ {t.start}</button></div><p className="muted selectable">{javaInfo}</p></section>}
            {page === 'plugins' && <AddonsPanel key={server.id} id={server.id} software={server.software} game={server.version} language={language} locked={locked} running={server.running} installedCore={server.installed} consent={pluginConsent} setConsent={setPluginConsent} act={act}/>}
            {page === 'files' && <FilesPanel key={server.id} id={server.id} language={language} locked={locked} running={server.running} act={act}/>}
            {page === 'worlds' && <WorldsPanel key={server.id} id={server.id} language={language} locked={locked} running={server.running} act={act}/>}
            {page === 'monitoring' && <MonitoringPanel key={server.id} id={server.id} names={Object.fromEntries(servers.map(s=>[s.id,s.name]))} language={language} locked={locked} customPath={javaPath} act={act}/>}
            {page === 'players' && <PlayersPanel key={server.id} id={server.id} language={language} locked={locked} running={server.running} act={act}/>}
            {page === 'properties' && <section className="panel"><h2>server.properties</h2><p className="muted">{t.propertiesHint}</p><textarea className="properties" spellCheck={false} value={properties} disabled={cannotEdit} onChange={e => setProperties(e.target.value)}/><div className="button-row"><button className="primary" disabled={cannotEdit} onClick={() => call('save_properties', {content:properties})}>{t.save}</button><button className="danger" disabled={cannotEdit} onClick={() => {const confirmation = window.prompt(`${t.typeDelete}\n${server.name}`); if (confirmation !== null) void call('delete_server', {confirmation});}}>{t.delete}</button></div></section>}
            {page === 'modpacks' && <ModpacksPanel key={server.id} id={server.id} language={language} locked={locked} running={server.running} act={act}/>}
            {page === 'backups' && <BackupsPanel key={server.id} id={server.id} serverName={server.name} language={language} locked={locked} running={server.running} act={act}/>}
            {page === 'network' && <NetworkPanel key={server.id} id={server.id} port={server.port} language={language} locked={locked} act={act}/>}
          </>}
        </>}
        <footer>MineHost <span>LOCAL FIRST. BUILT IN THE CLOUD.</span></footer>
      </div>
    </main>
    {creating && <div className="modal-backdrop"><form className="modal" onSubmit={e => {e.preventDefault(); void act(async () => {const id = await invoke<string>('create_server', {name,version,port,ramMb:ram,software,loaderVersion:loaderVersion||null}); setSelected(id); setCreating(false); setPage('software');});}}><p className="eyebrow">A NEW ADVENTURE</p><h2>{t.create}</h2><label>{language === 'ru' ? 'Ядро сервера' : 'Server software'}<select value={software} disabled={working} onChange={e => {setSoftware(e.target.value as Software); setVersions([]); setVersion('');}}>{(Object.keys(softwareLabels) as Software[]).map(core => <option key={core} value={core}>{softwareLabels[core]}</option>)}</select></label>{['fabric','forge','neoforge'].includes(software)&&<label>{language==='ru'?'Версия загрузчика (пусто — последняя стабильная)':'Loader version (empty for latest stable)'}<input value={loaderVersion} maxLength={40} placeholder="0.18.4 / 52.1.0 / 21.1.203" onChange={e=>setLoaderVersion(e.target.value)}/></label>}<label>{t.name}<input value={name} required maxLength={80} onChange={e => setName(e.target.value)}/></label><div className="button-row"><button type="button" disabled={working} onClick={loadVersions}>{t.loadVersions}</button></div><label>{t.version}<select required value={version} onChange={e => setVersion(e.target.value)}><option value="">—</option>{versions.map(v => <option key={v}>{v}</option>)}</select></label><div className="form-grid"><label>{t.port}<input type="number" required min={1024} max={65535} value={port} onChange={e => setPort(Number(e.target.value))}/></label><label>{t.ram}<input type="number" required min={512} max={12288} step={256} value={ram} onChange={e => setRam(Number(e.target.value))}/></label></div><div className="button-row"><button type="button" disabled={working} onClick={() => setCreating(false)}>{t.cancel}</button><button className="primary" disabled={working || !version}>{t.create}</button></div></form></div>}
  </div>;
}

createRoot(document.getElementById('root')!).render(<React.StrictMode><App/></React.StrictMode>);

import { useCallback, useEffect, useState } from 'react';
import { Channel, invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { DownloadStatus } from './DownloadStatus';
import { downloadedSize, type DownloadProgress } from './download';
import {GalleryPanel} from './GalleryPanel';

type Installed = {fileName: string; enabled: boolean; size: number; projectId: string | null; versionId: string | null; versionNumber: string | null};
type Hit = {project_id: string; title: string; description: string; downloads: number};
type Version = {id: string; name: string; version_number: string};
type PlanItem = {projectId: string; versionId: string; title: string; versionNumber: string; fileName: string; size: number};
type Props = {id: string; software: string; game: string; language: 'ru' | 'en'; locked: boolean; running: boolean; installedCore: boolean; consent: boolean; setConsent: (v: boolean) => void; act: (fn: () => Promise<unknown>) => Promise<void>};
const texts = {
  ru: {installed: 'Установленные дополнения', catalog: 'Каталог Modrinth', refresh: 'Обновить список', import: 'Импортировать JAR', search: 'Поиск', placeholder: 'Название мода или плагина', relevance: 'По релевантности', downloads: 'По загрузкам', updated: 'Недавно обновлённые', enabled: 'Включено', disabled: 'Отключено', enable: 'Включить', disable: 'Отключить', remove: 'Удалить', update: 'Проверить обновление', choose: 'Выбрать версию', versions: 'Совместимые стабильные версии', preview: 'Проверить зависимости', install: 'Установить', noVersions: 'Совместимых стабильных версий нет', empty: 'Дополнений пока нет', noResults: 'Проекты не найдены', stopped: 'Для установки, обновления и изменения файлов остановите сервер.', risk: 'Я доверяю выбранным файлам и понимаю, что моды и плагины выполняют код с правами моего пользователя.', previous: 'Назад', next: 'Далее', plan: 'План установки и обязательные зависимости', planHint: 'Все файлы проверяются по SHA-512. Предыдущие версии сохраняются в .minehost/addon-backups. Необязательные зависимости автоматически не устанавливаются.', cancel: 'Закрыть', latest: 'Уже установлена актуальная совместимая версия', deleteConfirm: 'Убрать дополнение из сервера? Файл останется в .minehost/addon-trash:', manual: 'Локальный импорт', project: 'Страница проекта', vanilla: 'Vanilla не поддерживает моды и плагины. Для дополнений создайте профиль Paper, Purpur или Fabric.', note: 'Показаны проекты для выбранной версии Minecraft и загрузчика. Совместимость ручных JAR определяется по доступным метаданным; работоспособность стороннего кода не гарантируется.'},
  en: {installed: 'Installed addons', catalog: 'Modrinth catalog', refresh: 'Refresh list', import: 'Import JAR', search: 'Search', placeholder: 'Mod or plugin name', relevance: 'Relevance', downloads: 'Downloads', updated: 'Recently updated', enabled: 'Enabled', disabled: 'Disabled', enable: 'Enable', disable: 'Disable', remove: 'Remove', update: 'Check for updates', choose: 'Choose version', versions: 'Compatible stable versions', preview: 'Check dependencies', install: 'Install', noVersions: 'No compatible stable releases', empty: 'No addons installed yet', noResults: 'No projects found', stopped: 'Stop the server before installing, updating or changing files.', risk: 'I trust these files and understand mods and plugins execute code with my user permissions.', previous: 'Previous', next: 'Next', plan: 'Installation plan and required dependencies', planHint: 'Every file is checked with SHA-512. Previous versions are retained in .minehost/addon-backups. Optional dependencies are not automatically installed.', cancel: 'Close', latest: 'The latest compatible version is already installed', deleteConfirm: 'Remove this addon from the server? The file will remain in .minehost/addon-trash:', manual: 'Local import', project: 'Project page', vanilla: 'Vanilla does not support mods or plugins. Create a Paper, Purpur or Fabric profile to use addons.', note: 'Projects are filtered by Minecraft version and loader. Local JAR compatibility is checked using available metadata; third-party code is not guaranteed to work.'}
};

export function AddonsPanel({id, software, game, language, locked, running, installedCore, consent, setConsent, act}: Props) {
  const t = texts[language];
  const [tab, setTab] = useState<'installed' | 'catalog'>('installed');
  const [installed, setInstalled] = useState<Installed[]>([]);
  const [query, setQuery] = useState('');
  const [sort, setSort] = useState('relevance');
  const [category,setCategory]=useState('');
  const [categories,setCategories]=useState<{name:string}[]>([]);
  useEffect(()=>{void invoke<{name:string}[]>('catalog_categories').then(setCategories).catch(()=>setCategories([]));},[]);
  const [hits, setHits] = useState<Hit[]>([]);
  const [offset, setOffset] = useState(0);
  const [total, setTotal] = useState(0);
  const [searched, setSearched] = useState(false);
  const [project, setProject] = useState<Hit | null>(null);
  const [versions, setVersions] = useState<Version[]>([]);
  const [version, setVersion] = useState('');
  const [plan, setPlan] = useState<{root: string; items: PlanItem[]} | null>(null);
  const [progress, setProgress] = useState<DownloadProgress | null>(null);
  const [loadError, setLoadError] = useState('');
  const refresh = useCallback(async () => {setInstalled(await invoke<Installed[]>('list_addons', {id})); setLoadError('');}, [id]);
  useEffect(() => {if (!locked) void refresh().catch(e => setLoadError(String(e)));}, [refresh, locked]);
  const run = (task: () => Promise<unknown>) => {void act(async () => {const result = await task(); await refresh(); return result;});};
  const editable = !locked && !running && installedCore;
  const search = (position = 0) => {void act(async () => {
    const response = await invoke<{hits: Hit[]; total_hits: number}>('search_addons', {id, query, offset: position, sort, category:category||null});
    setHits(response.hits); setTotal(response.total_hits); setOffset(position); setSearched(true);
  });};
  const preview = async (root: string) => {setPlan({root, items: await invoke<PlanItem[]>('plan_addon', {id, versionId: root})});};
  const choose = (hit: Hit) => {void act(async () => {
    const values = await invoke<Version[]>('addon_versions', {id, projectId: hit.project_id});
    setProject(hit); setVersions(values); setVersion(values[0]?.id ?? ''); setPlan(null);
  });};
  const install = () => {if (!plan || !consent) return; const approved = plan;
    run(async () => {
      setProgress({phase: 'resolving', downloaded: 0, total: null});
      const onProgress = new Channel<DownloadProgress>(); let failed = false;
      onProgress.onmessage = event => {if (!failed) setProgress(event);};
      try {await invoke('install_addon', {id, versionId: approved.root, approvedVersions: approved.items.map(v => v.versionId), consent, onProgress}); setPlan(null);}
      catch (e) {failed = true; setProgress(current => ({phase: 'failed', downloaded: current?.downloaded ?? 0, total: current?.total ?? null, error: String(e)})); throw e;}
    });
  };
  const importFiles = () => run(async () => {
    const files = await open({multiple: true, directory: false, filters: [{name: 'JAR', extensions: ['jar']}]});
    for (const source of typeof files === 'string' ? [files] : files ?? []) await invoke('import_addon', {id, source, consent});
  });
  const update = (addon: Installed) => {void act(async () => {
    const values = await invoke<Version[]>('addon_versions', {id, projectId: addon.projectId});
    if (!values.length) throw new Error(t.noVersions);
    if (values[0].id === addon.versionId) return t.latest;
    await preview(values[0].id);
  });};
  if (software === 'vanilla') return <section className="panel"><h2>{t.installed}</h2><p className="muted">{t.vanilla}</p></section>;
  return <section className="panel addons-panel">
    <div className="panel-heading"><h2>{software.toUpperCase()} · Minecraft {game}</h2><button disabled={locked} onClick={() => run(refresh)}>{t.refresh}</button></div>
    <p className="muted">{t.stopped}</p>
    {!installedCore && <p className="muted">{language === 'ru' ? 'Сначала установите выбранное ядро в разделе «Сервер и Java».' : 'Install the selected server software in Server & Java first.'}</p>}
    <label className="check"><input type="checkbox" checked={consent} onChange={e => setConsent(e.target.checked)}/>{t.risk}</label>
    <div className="button-row addon-tabs"><button className={tab === 'installed' ? 'primary' : ''} onClick={() => setTab('installed')}>{t.installed} · {installed.length}</button><button className={tab === 'catalog' ? 'primary' : ''} onClick={() => setTab('catalog')}>{t.catalog}</button><button disabled={!editable || !consent} onClick={importFiles}>{t.import}</button></div>
    {loadError && <p role="alert" className="addon-error">{loadError}</p>}
    {progress && <DownloadStatus progress={progress} language={language}/>}
    {plan && <div className="install-plan"><h3>{t.plan}</h3><p className="muted">{t.planHint}</p>{plan.items.map(item => <div key={item.versionId} className="plan-item"><strong>{item.title} <small>{item.versionNumber}</small></strong><span>{item.fileName} · {downloadedSize(item.size, language)}</span></div>)}<div className="button-row"><button className="primary" disabled={!editable || !consent} onClick={install}>{t.install} · {plan.items.length}</button><button disabled={locked} onClick={() => setPlan(null)}>{t.cancel}</button></div></div>}
    {tab === 'installed' ? <div className="addon-list">{!installed.length && <p className="muted">{t.empty}</p>}{installed.map(addon => <article className="addon-row" key={addon.fileName}><div><strong>{addon.fileName}</strong><p className="muted">{addon.enabled ? t.enabled : t.disabled} · {downloadedSize(addon.size, language)} · {addon.versionNumber ?? t.manual}</p></div><div className="button-row"><button disabled={!editable} onClick={() => run(() => invoke('set_addon_enabled', {id, fileName: addon.fileName, enabled: !addon.enabled}))}>{addon.enabled ? t.disable : t.enable}</button>{addon.projectId && <button disabled={!editable} onClick={() => update(addon)}>{t.update}</button>}<button className="danger" disabled={!editable} onClick={() => {if(window.confirm(`${t.deleteConfirm}\n${addon.fileName}`)) run(() => invoke('remove_addon', {id, fileName: addon.fileName, confirmation: addon.fileName}));}}>{t.remove}</button></div></article>)}</div> : <>
      <form className="catalog-search" onSubmit={e => {e.preventDefault(); search();}}><input aria-label={t.search} maxLength={100} placeholder={t.placeholder} value={query} onChange={e => setQuery(e.target.value)}/><select aria-label={t.relevance} value={sort} onChange={e => setSort(e.target.value)}><option value="relevance">{t.relevance}</option><option value="downloads">{t.downloads}</option><option value="updated">{t.updated}</option></select><select aria-label={language === 'ru' ? 'Категория' : 'Category'} value={category} onChange={e=>setCategory(e.target.value)}><option value="">{language === 'ru' ? 'Все категории' : 'All categories'}</option>{categories.map(v=><option key={v.name}>{v.name}</option>)}</select><button className="primary" disabled={locked}>{t.search}</button></form>
      <p className="muted">{t.note}</p>{searched && !hits.length && <p className="muted">{t.noResults}</p>}
      <div className="catalog-grid">{hits.map(hit => <article className="catalog-card" key={hit.project_id}><h3>{hit.title}</h3><p className="muted">{hit.description}</p><small>{hit.downloads.toLocaleString()} ↓</small><div className="button-row"><button disabled={locked} onClick={() => choose(hit)}>{t.choose}</button><button disabled={locked} onClick={() => {void act(() => invoke('open_addon_project', {projectId: hit.project_id}));}}>{t.project}</button></div></article>)}</div>
      {searched && <div className="catalog-pages"><button disabled={locked || offset === 0} onClick={() => search(Math.max(0, offset - 20))}>{t.previous}</button><span>{offset + 1}–{offset + hits.length} / {total.toLocaleString()}</span><button disabled={locked || offset + 20 >= total || offset >= 10000} onClick={() => search(offset + 20)}>{t.next}</button></div>}
      {project && <GalleryPanel projectId={project.project_id} language={language}/>}
      {project && <div className="install-plan"><h3>{project.title}</h3><label>{t.versions}<select value={version} onChange={e => setVersion(e.target.value)} disabled={locked || !versions.length}>{!versions.length && <option value="">{t.noVersions}</option>}{versions.map(v => <option key={v.id} value={v.id}>{v.name} · {v.version_number}</option>)}</select></label><button disabled={locked || !version} onClick={() => {void act(() => preview(version));}}>{t.preview}</button></div>}
    </>}
  </section>;
}

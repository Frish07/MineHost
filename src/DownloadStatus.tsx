import { downloadLabels, downloadPercentage, downloadedSize, type DownloadProgress } from './download';

export function DownloadStatus({progress, language}: {progress: DownloadProgress; language: 'ru' | 'en'}) {
  const labels = downloadLabels[language];
  const percent = downloadPercentage(progress);
  const amount = downloadedSize(progress.downloaded, language);
  const total = progress.total === null ? '' : ` / ${downloadedSize(progress.total, language)}`;
  return <div className={`download-status ${progress.phase === 'failed' ? 'failed' : ''}`}>
    <div className="download-heading"><strong role="status">{labels[progress.phase]}</strong><span>{percent === undefined ? '…' : `${percent}%`}</span></div>
    <progress aria-label={labels.title} aria-valuetext={`${labels[progress.phase]} ${amount}${total}`} max={100} value={percent}/>
    <div className="download-details">{amount}{total}</div>
    {progress.error && <p role="alert">{progress.error}</p>}
  </div>;
}

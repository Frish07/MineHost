export type DownloadPhase = 'resolving' | 'downloading' | 'verifying' | 'installing' | 'complete' | 'failed';

export type DownloadProgress = {
  phase: DownloadPhase;
  downloaded: number;
  total: number | null;
  error?: string;
};

export const downloadLabels = {
  ru: { resolving: 'Получение данных Paper…', downloading: 'Скачивание Paper…', verifying: 'Проверка SHA-256…', installing: 'Сохранение файлов…', complete: 'Paper установлен', failed: 'Не удалось установить Paper', title: 'Загрузка Paper' },
  en: { resolving: 'Getting Paper metadata…', downloading: 'Downloading Paper…', verifying: 'Verifying SHA-256…', installing: 'Saving files…', complete: 'Paper installed', failed: 'Paper installation failed', title: 'Paper download' },
};

export function downloadPercentage(progress: DownloadProgress): number | undefined {
  if (progress.phase === 'complete') return 100;
  if (progress.total === null || progress.total <= 0 || !Number.isFinite(progress.total)) return undefined;
  return Math.max(0, Math.min(100, Math.floor(progress.downloaded / progress.total * 100)));
}

export function downloadedSize(bytes: number, language: 'ru' | 'en'): string {
  const number = new Intl.NumberFormat(language === 'ru' ? 'ru-RU' : 'en-US', { maximumFractionDigits: 1 });
  return `${number.format(bytes / (1024 * 1024))} ${language === 'ru' ? 'МиБ' : 'MiB'}`;
}

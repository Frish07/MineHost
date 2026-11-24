export type DownloadPhase = 'resolving' | 'downloading' | 'verifying' | 'installing' | 'complete' | 'failed';

export type DownloadProgress = {
  phase: DownloadPhase;
  downloaded: number;
  total: number | null;
  error?: string;
};

export const downloadLabels = {
  ru: { resolving: 'Получение данных…', downloading: 'Скачивание…', verifying: 'Проверка файла…', installing: 'Установка и подготовка файлов…', complete: 'Установлено', failed: 'Не удалось установить', title: 'Загрузка' },
  en: { resolving: 'Getting metadata…', downloading: 'Downloading…', verifying: 'Verifying file…', installing: 'Installing and preparing files…', complete: 'Installed', failed: 'Installation failed', title: 'Download' },
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

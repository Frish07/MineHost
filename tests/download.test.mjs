import test from 'node:test';
import assert from 'node:assert/strict';
import { downloadPercentage, downloadedSize } from '../src/download.ts';

test('unknown lengths stay indeterminate instead of showing misleading percentages', () => {
  for (const total of [null, 0, -1, Infinity]) {
    assert.equal(downloadPercentage({phase: 'downloading', downloaded: 1024, total}), undefined);
  }
});

test('percentage is bounded and completion requires its own phase', () => {
  assert.equal(downloadPercentage({phase: 'downloading', downloaded: 25, total: 100}), 25);
  assert.equal(downloadPercentage({phase: 'downloading', downloaded: 125, total: 100}), 100);
  assert.equal(downloadPercentage({phase: 'downloading', downloaded: -1, total: 100}), 0);
  assert.equal(downloadPercentage({phase: 'complete', downloaded: 100, total: null}), 100);
});

test('downloaded amounts are shown in MiB with the selected language', () => {
  assert.equal(downloadedSize(2 * 1024 * 1024, 'en'), '2 MiB');
  assert.equal(downloadedSize(2 * 1024 * 1024, 'ru'), '2 МиБ');
});

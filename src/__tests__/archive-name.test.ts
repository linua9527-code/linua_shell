import { describe, expect, it } from 'vitest';
import { archiveFileName, isValidArchiveName } from '@/lib/archive-name';

describe('archive names', () => {
  it('normalizes ZIP extensions without duplicating them', () => {
    expect(archiveFileName('project')).toBe('project.zip');
    expect(archiveFileName('project.ZIP')).toBe('project.ZIP');
  });

  it('accepts ordinary names and rejects path traversal and path separators', () => {
    expect(isValidArchiveName('项目 1.zip')).toBe(true);
    for (const name of ['', '.', '..', '../outside', 'folder/file', 'folder\\file', 'C:drive']) {
      expect(isValidArchiveName(name)).toBe(false);
    }
  });
});

export function archiveFileName(name: string): string {
  return name.toLowerCase().endsWith('.zip') ? name : `${name}.zip`;
}

export function isValidArchiveName(name: string): boolean {
  return name.length > 0 && name !== '.' && name !== '..' &&
    !/[\\/<>:"|?*]/.test(name) &&
    ![...name].some((character) => character.charCodeAt(0) < 32);
}

/** Stable identity: changing a bot's name or state never changes its silhouette. */
export function botShape(id: string) {
  let hash = 2166136261;
  for (const char of id) hash = Math.imul(hash ^ char.charCodeAt(0), 16777619) >>> 0;
  const variants = [
    'M16 38C12 26 19 12 30 16C40 5 56 16 54 28C67 37 58 56 47 55C36 65 15 56 16 38Z',
    'M12 40C10 29 21 27 25 15C28 6 40 10 44 20C47 28 60 28 60 40C60 53 47 55 38 53C25 61 11 53 12 40Z',
    'M14 34C14 20 22 12 35 14C50 10 59 23 56 36L59 49C60 58 48 60 43 54C32 61 18 57 16 48Z',
    'M13 33C10 22 23 12 33 18C44 6 58 17 55 31C65 42 54 55 43 53C32 64 18 56 19 47C10 46 9 39 13 33Z'
  ];
  const path = variants[hash % variants.length].replace(/\d+/g, (value, index: number) => {
    const offset = ((hash >>> (index % 24)) % 9 - 4) * 0.24;
    return (Number(value) + offset).toFixed(2);
  });
  return { hash, path, eyeY: 33 + (hash % 3), eyeGap: 10 + (hash % 4) };
}

export function savedValue(key: string): unknown {
  try {
    return JSON.parse(sessionStorage.getItem(`reviewq.v1.${key}`) ?? "null");
  } catch {
    return null;
  }
}

export function saveValue(key: string, value: unknown) {
  try {
    sessionStorage.setItem(`reviewq.v1.${key}`, JSON.stringify(value));
  } catch {
    // Optional per-tab presentation state.
  }
}

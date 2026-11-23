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
    return true;
  } catch {
    return false;
  }
}

export function removeValue(key: string) {
  try {
    sessionStorage.removeItem(`reviewq.v1.${key}`);
  } catch {
    // Optional per-tab presentation state.
  }
}

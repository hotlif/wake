function isPlainObject(value) {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

export function equalValue(left, right) {
  if (Object.is(left, right)) return true;
  try {
    return JSON.stringify(left) === JSON.stringify(right);
  } catch {
    return false;
  }
}

export function changedArgs(args, defaults) {
  return Object.fromEntries(
    Object.entries(args).filter(([name, value]) => !equalValue(value, defaults[name])),
  );
}

export function unsetArgs(args, defaults) {
  return Object.keys(defaults).filter((name) => !Object.prototype.hasOwnProperty.call(args, name));
}

export function applyLocationArgs(defaults, location) {
  const next = { ...defaults, ...location.args };
  location.unset.forEach((name) => delete next[name]);
  return next;
}

export function readLocationHash(hash) {
  const fallback = { args: {}, unset: [], viewport: "responsive" };
  const match = hash.match(/^#\/components\/([^?]*)(?:\?(.*))?$/);
  if (!match) return fallback;

  let id;
  try {
    id = decodeURIComponent(match[1]);
  } catch {
    return fallback;
  }

  let args = {};
  let unset = [];
  const params = new URLSearchParams(match[2] || "");
  try {
    const parsed = JSON.parse(params.get("args") || "{}");
    if (isPlainObject(parsed)) args = parsed;
  } catch {
    args = {};
  }
  try {
    const parsed = JSON.parse(params.get("unset") || "[]");
    if (Array.isArray(parsed)) {
      unset = Array.from(new Set(parsed.filter((name) => typeof name === "string")));
    }
  } catch {
    unset = [];
  }

  const rawViewport = params.get("viewport");
  const viewport = rawViewport === "tablet" || rawViewport === "mobile"
    ? rawViewport
    : "responsive";
  return { id, args, unset, viewport };
}

export function locationHash(id, args, defaults, viewport) {
  const params = new URLSearchParams();
  const changed = changedArgs(args, defaults);
  const unset = unsetArgs(args, defaults);
  if (Object.keys(changed).length) params.set("args", JSON.stringify(changed));
  if (unset.length) params.set("unset", JSON.stringify(unset));
  if (viewport !== "responsive") params.set("viewport", viewport);
  const query = params.toString();
  return "#/components/" + encodeURIComponent(id) + (query ? "?" + query : "");
}

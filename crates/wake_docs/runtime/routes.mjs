const UNRESERVED = /^[A-Za-z0-9._~-]$/;
const utf8 = new TextEncoder();

export function encodeRouteSegment(value) {
  let encoded = "";
  for (const byte of utf8.encode(value)) {
    const character = String.fromCharCode(byte);
    if (UNRESERVED.test(character)) encoded += character;
    else encoded += `%${byte.toString(16).toUpperCase().padStart(2, "0")}`;
  }
  return encoded;
}

export function canonicalRoutePath(pathname) {
  let value = String(pathname || "/");
  if (!value.startsWith("/")) value = "/" + value;
  value = value.replace(/\/+$/, "") || "/";
  if (value === "/") return { decoded: "/", encoded: "/" };

  const decodedSegments = [];
  const encodedSegments = [];
  for (const segment of value.slice(1).split("/")) {
    if (!segment) return null;
    let decoded;
    try {
      decoded = decodeURIComponent(segment);
    } catch {
      return null;
    }
    if (!decoded || decoded === "." || decoded === ".." || decoded.includes("/") || decoded.includes("\\")) {
      return null;
    }
    decodedSegments.push(decoded);
    encodedSegments.push(encodeRouteSegment(decoded));
  }
  return {
    decoded: "/" + decodedSegments.join("/"),
    encoded: "/" + encodedSegments.join("/"),
  };
}

export function routePathFromDecoded(pathname) {
  let value = String(pathname || "/");
  if (!value.startsWith("/")) value = "/" + value;
  value = value.replace(/\/+$/, "") || "/";
  if (value === "/") return { decoded: "/", encoded: "/" };

  const decodedSegments = value.slice(1).split("/");
  if (decodedSegments.some((segment) => !segment || segment === "." || segment === ".." || segment.includes("\\"))) {
    return null;
  }
  return {
    decoded: "/" + decodedSegments.join("/"),
    encoded: "/" + decodedSegments.map(encodeRouteSegment).join("/"),
  };
}

export function routePathFromLocation(basePath, pathname) {
  const base = canonicalRoutePath(basePath);
  const location = canonicalRoutePath(pathname);
  if (!base || !location) return null;
  if (base.encoded === "/") return location;
  if (location.encoded === base.encoded) return { decoded: "/", encoded: "/" };
  if (!location.encoded.startsWith(base.encoded + "/")) return null;
  return canonicalRoutePath(location.encoded.slice(base.encoded.length));
}

export function docsRouteHref(basePath, route) {
  const base = canonicalRoutePath(basePath);
  const target = canonicalRoutePath(route);
  if (!base || !target) return null;
  if (base.encoded === "/") return target.encoded;
  return target.encoded === "/" ? base.encoded + "/" : base.encoded + target.encoded;
}

export function findPageForPath(pages, pathname) {
  const routePath = canonicalRoutePath(pathname);
  if (!routePath) return undefined;
  return pages.find((page) => page.routePath.encoded === routePath.encoded);
}

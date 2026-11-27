/** Match the native handler's lowercase UUID grammar before constructing the URI. */
export function desktopUri(fragment) {
  if (fragment.length !== 37 || !/^#[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}$/.test(fragment)) return null;
  return `praxi://chat/${fragment.slice(1)}`;
}

if (typeof document !== "undefined") {
  const link = document.getElementById("open-app");
  const uri = desktopUri(location.hash);
  if (link && uri) {
    link.setAttribute("href", uri);
    link.className = "btn primary";
    link.hidden = false;
  }
}

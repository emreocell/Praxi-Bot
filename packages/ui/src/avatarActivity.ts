/**
 * Keeps avatar animations free when nobody can see them:
 * - the whole document pauses (`data-avatar-paused` on <html>) while the window is hidden;
 * - each avatar pauses (`data-offscreen`) while scrolled out of view.
 * One shared IntersectionObserver and one visibilitychange listener serve every avatar.
 */
let observer: IntersectionObserver | null = null;
let visibilityListening = false;
let users = 0;

function syncVisibility(): void {
  if (typeof document === 'undefined') return;
  if (document.hidden) {
    document.documentElement.setAttribute('data-avatar-paused', 'true');
  } else {
    document.documentElement.removeAttribute('data-avatar-paused');
  }
}

function getObserver(): IntersectionObserver | null {
  if (observer || typeof IntersectionObserver === 'undefined') return observer;
  observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        if (entry.isIntersecting) {
          entry.target.removeAttribute('data-offscreen');
        } else {
          entry.target.setAttribute('data-offscreen', 'true');
        }
      }
    },
    { rootMargin: '48px' }
  );
  return observer;
}

/** Start pausing animation of `el` while hidden/off-screen. Returns the cleanup. */
export function trackAvatarActivity(el: HTMLElement): () => void {
  if (typeof document === 'undefined') return () => undefined;
  users += 1;
  if (!visibilityListening) {
    visibilityListening = true;
    document.addEventListener('visibilitychange', syncVisibility);
  }
  syncVisibility();
  const obs = getObserver();
  obs?.observe(el);
  return () => {
    obs?.unobserve(el);
    el.removeAttribute('data-offscreen');
    users -= 1;
    if (users <= 0) {
      users = 0;
      if (visibilityListening) {
        visibilityListening = false;
        document.removeEventListener('visibilitychange', syncVisibility);
      }
      observer?.disconnect();
      observer = null;
      document.documentElement.removeAttribute('data-avatar-paused');
    }
  };
}

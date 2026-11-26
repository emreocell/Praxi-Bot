import { siGithub, siGitlab, siNotion, siGoogledrive, siGooglecalendar, siGmail, siDiscord, siFigma, siLinear, siAnthropic, siGooglegemini, siOllama, siClaude, siPostgresql, siSqlite, siBrave, siPuppeteer, siSentry, siTodoist, siObsidian, siHomeassistant, siOpenrouter, siDeepseek, siMistralai, siCursor, siGithubcopilot, siLmstudio, siOpencode, siStripe, siAtlassian, siJira, siConfluence, siTrello, siAsana, siHubspot, siShopify, siDropbox, siZoom, siX, siFacebook, siInstagram, siYoutube, siReddit, siTelegram, siWhatsapp, siSpotify, siAirtable, siClickup, siZendesk, siIntercom, siMailchimp, siCalendly, siDocker, siVercel, siNetlify, siSupabase, siCloudflare, siGooglesheets, siGoogledocs, siGoogleslides, siGooglemeet, siGooglemaps, siGooglephotos, siBitbucket, siDatadog, siPagerduty, siMongodb, siRedis, siMiro, siBox, siEvernote, siPaypal, siSquare, siQuickbooks, siXero, siWordpress, siMedium, siGhost, siSubstack, siZapier, siMake, siN8n, siResend, siMixpanel, siPosthog, siFirebase, siDigitalocean, siQwen, siKimi, siTiktok } from 'simple-icons';

const icons = [siGithub, siGitlab, siNotion, siGoogledrive, siGooglecalendar, siGmail, siDiscord, siFigma, siLinear, siAnthropic, siGooglegemini, siOllama, siClaude, siPostgresql, siSqlite, siBrave, siPuppeteer, siSentry, siTodoist, siObsidian, siHomeassistant, siOpenrouter, siDeepseek, siMistralai, siCursor, siGithubcopilot, siLmstudio, siOpencode, siStripe, siAtlassian, siJira, siConfluence, siTrello, siAsana, siHubspot, siShopify, siDropbox, siZoom, siX, siFacebook, siInstagram, siYoutube, siReddit, siTelegram, siWhatsapp, siSpotify, siAirtable, siClickup, siZendesk, siIntercom, siMailchimp, siCalendly, siDocker, siVercel, siNetlify, siSupabase, siCloudflare, siGooglesheets, siGoogledocs, siGoogleslides, siGooglemeet, siGooglemaps, siGooglephotos, siBitbucket, siDatadog, siPagerduty, siMongodb, siRedis, siMiro, siBox, siEvernote, siPaypal, siSquare, siQuickbooks, siXero, siWordpress, siMedium, siGhost, siSubstack, siZapier, siMake, siN8n, siResend, siMixpanel, siPosthog, siFirebase, siDigitalocean, siQwen, siKimi, siTiktok];
const normalize = (s: string) => s.toLowerCase().replace(/[^a-z0-9]/g, '');
const aliases: Record<string, string> = {
  'bravesearch': 'brave',
  postgres: 'postgresql',
  gemini: 'googlegemini',
  geminicli: 'googlegemini',
  cligemini: 'googlegemini',
  geminiopenaicompat: 'googlegemini',
  mistral: 'mistralai',
  togetherai: 'together',
  clicursor: 'cursor',
  cursoragent: 'cursor',
  claudecode: 'claude',
  claudecodecli: 'claude',
  cliclaude: 'claude',
  codex: 'openai',
  clicodex: 'openai',
  chatgptplan: 'openai',
  google: 'googlegemini',
  notionremote: 'notion',
  twitter: 'x',
  googlecal: 'googlecalendar',
  atlassianjira: 'jira',
  jiracloud: 'jira',
  agy: 'antigravity',
  cliagy: 'antigravity',
  cliqwen: 'qwen',
  clikiro: 'kiro',
  cliqoder: 'qoder',
  qodercli: 'qoder',
  kirocli: 'kiro',
  copilot: 'githubcopilot',
  clicopilot: 'githubcopilot',
  clikimi: 'kimi',
  cligoose: 'goose',
  // Mistral Vibe is Mistral AI's own CLI: the Mistral AI mark.
  vibe: 'mistralai',
  clivibe: 'mistralai',
  vibeacp: 'mistralai',
  mistralvibe: 'mistralai',
  cliopencode: 'opencode'
};

// Explicit entries, not invented logos. See docs/design/brand-icons.md for the
// licensing follow-up. `known` means bundled, so AppLogo never fetches these.
const initials: Record<string, string> = {
  groq: 'Groq',
  together: 'Together AI',
  llamacpp: 'llama.cpp',
  playwright: 'Playwright',
  exa: 'Exa',
  tavily: 'Tavily',
  firecrawl: 'Firecrawl',
  context7: 'Context7'
};
const monochromeBrands = new Set([
  'anthropic', 'claude', 'openai', 'openaicodex', 'googlegemini',
  'cursor', 'opencode', 'antigravity', 'ollama', 'lmstudio',
  'openrouter', 'deepseek', 'mistralai', 'githubcopilot', 'qwen', 'kimi'
]);

interface BrandArtwork {
  name: string;
  color: string;
  path: string;
  monogram: string;
  /** Bundled artwork, including an explicitly documented neutral initial. */
  known: boolean;
  /** Follow the theme's text colour on both bare marks and tiles. */
  monochrome?: boolean;
}

export function brandIcon(name: string): BrandArtwork {
  const key = aliases[normalize(name)] ?? normalize(name);
  const brand = icons.find(i => key === i.slug || key === normalize(i.title));
  if (brand) return { name: brand.title, color: `#${brand.hex}`, path: brand.path, monogram: '', known: true, monochrome: monochromeBrands.has(key) };
  // Legacy CC0 OpenAI path from simple-icons 15.0.0; Slack uses local multicolor art.
  if (key === 'openai' || key === 'openaicodex') return { name: 'OpenAI', color: '#10A37F', path: "M22.2819 9.8211a5.9847 5.9847 0 0 0-.5157-4.9108 6.0462 6.0462 0 0 0-6.5098-2.9A6.0651 6.0651 0 0 0 4.9807 4.1818a5.9847 5.9847 0 0 0-3.9977 2.9 6.0462 6.0462 0 0 0 .7427 7.0966 5.98 5.98 0 0 0 .511 4.9107 6.051 6.051 0 0 0 6.5146 2.9001A5.9847 5.9847 0 0 0 13.2599 24a6.0557 6.0557 0 0 0 5.7718-4.2058 5.9894 5.9894 0 0 0 3.9977-2.9001 6.0557 6.0557 0 0 0-.7475-7.0729zm-9.022 12.6081a4.4755 4.4755 0 0 1-2.8764-1.0408l.1419-.0804 4.7783-2.7582a.7948.7948 0 0 0 .3927-.6813v-6.7369l2.02 1.1686a.071.071 0 0 1 .038.052v5.5826a4.504 4.504 0 0 1-4.4945 4.4944zm-9.6607-4.1254a4.4708 4.4708 0 0 1-.5346-3.0137l.142.0852 4.783 2.7582a.7712.7712 0 0 0 .7806 0l5.8428-3.3685v2.3324a.0804.0804 0 0 1-.0332.0615L9.74 19.9502a4.4992 4.4992 0 0 1-6.1408-1.6464zM2.3408 7.8956a4.485 4.485 0 0 1 2.3655-1.9728V11.6a.7664.7664 0 0 0 .3879.6765l5.8144 3.3543-2.0201 1.1685a.0757.0757 0 0 1-.071 0l-4.8303-2.7865A4.504 4.504 0 0 1 2.3408 7.872zm16.5963 3.8558L13.1038 8.364 15.1192 7.2a.0757.0757 0 0 1 .071 0l4.8303 2.7913a4.4944 4.4944 0 0 1-.6765 8.1042v-5.6772a.79.79 0 0 0-.407-.667zm2.0107-3.0231l-.142-.0852-4.7735-2.7818a.7759.7759 0 0 0-.7854 0L9.409 9.2297V6.8974a.0662.0662 0 0 1 .0284-.0615l4.8303-2.7866a4.4992 4.4992 0 0 1 6.6802 4.66zM8.3065 12.863l-2.02-1.1638a.0804.0804 0 0 1-.038-.0567V6.0742a4.4992 4.4992 0 0 1 7.3757-3.4537l-.142.0805L8.704 5.459a.7948.7948 0 0 0-.3927.6813zm1.0976-2.3654l2.602-1.4998 2.6069 1.4998v2.9994l-2.5974 1.4997-2.6067-1.4997Z", monogram: '', known: true, monochrome: true };
  // Official one-colour press SVG, rescaled from 180 to 24; see assets/brands/antigravity.svg.
  if (key === 'antigravity') return { name: 'Antigravity', color: '#202124', path: 'M19.23306667 19.87493333C20.23306667 20.62506667 21.73306667 20.12506667 20.358 18.75C16.23306667 14.75 17.108 3.75 11.98302667 3.75C6.85802667 3.75 7.73302667 14.75 3.60802667 18.75C2.10802667 20.25 3.73302667 20.62506667 4.73302667 19.87493333C8.60802667 17.25 8.35802667 12.625 11.98302667 12.625C15.608 12.625 15.358 17.25 19.23306667 19.87493333Z', monogram: '', known: true, monochrome: true };
  // Official Kiro and Qoder marks, taken from each vendor's own published icon (no press kit or Simple Icons entry exists)
  // and reduced to one monochrome path in a 24-box; see docs/design/brand-icons.md and src/assets/brands/{kiro,qoder}.svg.
  if (key === 'kiro') return { name: 'Kiro CLI', color: '#9046FF', path: 'M6.94 17.5C4.87 22.08 9.28 23.23 12.53 20.55C13.48 23.56 17.06 21.31 18.35 18.98C21.18 13.85 20.04 8.61 19.75 7.53C17.73 .16 7.67 .15 5.94 7.56C5.53 8.86 5.53 10.34 5.3 11.87C5.19 12.65 5.1 13.14 4.81 13.95C4.63 14.42 4.4 14.83 4.03 15.53C3.45 16.62 3.7 18.71 6.67 17.63L6.95 17.5L6.94 17.5ZM12.92 10.72C12.09 10.72 11.97 9.73 11.97 9.15C11.97 8.61 12.06 8.2 12.24 7.93C12.4 7.69 12.63 7.57 12.92 7.57C13.2 7.57 13.45 7.69 13.63 7.93C13.83 8.21 13.93 8.63 13.93 9.15C13.93 10.13 13.55 10.72 12.92 10.72L12.92 10.72ZM16.31 10.72C15.49 10.72 15.36 9.73 15.36 9.15C15.36 8.61 15.46 8.2 15.64 7.93C15.8 7.69 16.03 7.57 16.31 7.57C16.6 7.57 16.85 7.69 17.03 7.93C17.23 8.21 17.33 8.63 17.33 9.15C17.33 10.13 16.95 10.72 16.32 10.72L16.31 10.72Z', monogram: '', known: true, monochrome: true };
  if (key === 'qoder') return { name: 'Qoder CLI', color: '#0F0D0C', path: 'M3.43 6.05l.79-1.1l.89-.93l1.02-.78l1.1-.59l.64-.24l.87-.24l1.07-.16l.55 0l.58 .05l.54 .12l.52 .19l7.22 3.78l.72 .48l.6 .57l.39 .54l.33 .61l.25 .64l.16 .69l.09 .92l0 3.31l-.02 5.79l-.08 .42l-.16 .37l-.22 .33l-.31 .3l-.37 .25l-.39 .18l-.39 .09l-.4 .02l-.69-.16l-1.19-.54l-.66 .34l-.8 .3l-1.15 .3l-.76 .09l-.65 0l-.48-.06l-.55-.12l-.51-.18l-7.44-3.91l-.49-.35l-.41-.37l-.36-.43l-.35-.54l-.43-.99l-.23-1.1l-.03-.71l0-3.42l.13-1.03l.22-.89l.38-.96zM14.3 12.88l-.6 1.26l-.8 1.16l-.94 .99l-1.06 .8l.08 .67l.24 .78l.29 .49l.53 .53l.56 .34l.62 .2l.63 .07l.72-.06l1.28-.36l.56-.25l.71-.42l.28-.07l.4 .06l1.63 .74l.24-.02l.21-.19l.05-.19l0-8.89l-.15-.97l-.15-.42l-.2-.38l-.25-.34l-.3-.29l-.58-.38l-.36-.15l-.41-.11l-.78-.04l-.77 .11l-1.17 .37l-.05 3.02l-.17 .98zM9.19 18.11l-.13-1.12l.03-3.86l.1-.72l.14-.62l.49-1.31l.43-.82l.51-.75l.58-.7l.61-.6l.1-.09l.16 .18l-.16-.18l.9-.68l.99-.54l.97-.35l-3.63-1.9l.1-.22l-.1 .22l-.13-.05l.08-.23l-.08 .23l-.57-.14l.03-.24l-.03 .24l-.2-.02l.02-.24l-.02 .24l-.24-.01l.13-.04l.09-.09l.02-.1l-.24-.01l0 .24l-.4 .02l-.02-.24l.02 .24l-.57 .1l-1.01 .3l-.54 .24l-.55 .32l-.13-.2l.13 .2l-.62 .47l-.46 .43l-.17-.17l.17 .17l-.56 .66l-.41 .59l-.38 .73l-.31 .79l-.23-.08l.23 .08l-.16 .63l-.1 .79l.01 3.64l.11 .74l.29 .78l.42 .6l.54 .46zM12.96 9.17l-.62 .69l-.51 .74l-.37 .69l-.31 .82l-.23 1.14l-.03 1.53l.5-.53l.39-.5l.4-.65l.28-.56l.25-.66l.13-.52l.1-.62zM9.56 18.57l-.09-.31l-.28-.15l.07 .31zM10.38 3.6l-.06-.16l-.18 .15l.11 0zM10.77 3.89l.01-.04l-.01-.2l-.05 .23zM9.44 11.43l.23 .08l-.22-.09zM4.77 7.41l-.22-.1l.22 .11zM4.45 7.87l-.13-.06zM4.01 9.09l.16 .02zM6.45 5.28l-.16-.18zM5.65 16.24l-.11 .21l.11-.21zM4.34 8.44l-.22-.08zM9.59 3.87l-.03-.24zM10.37 3.84l.02-.24zM8.16 4.25l-.08-.23zM6.82 4.99l-.14-.2zM4.74 7.47l-.21-.11l.21 .11zM5.02 6.96l-.2-.12l.2 .12zM5.77 5.95l-.18-.16zM12.06 7.51l0 0l.11 .14zM5.22 3.93l-.01 0l.06 .06zM3.63 6.16l-.01-.01zM12.7 19.94l-.01 0z', monogram: '', known: true, monochrome: true };
  // No licensed Goose mark: the same neutral terminal-prompt glyph.
  if (key === 'goose') return { name: 'Goose', color: 'currentColor', path: 'M3.5 6.5 5 5l7 7-7 7-1.5-1.5L9 12ZM12 17h8.5v2H12Z', monogram: '', known: true, monochrome: true };
  if (initials[key]) return { name: initials[key], color: 'currentColor', path: '', monogram: initials[key][0].toUpperCase(), known: true, monochrome: true };
  if (key === 'slack') return { name: 'Slack', color: '#4A154B', path: '', monogram: '', known: true };
  let hash = 0;
  for (const char of key) hash = (Math.imul(hash, 31) + char.charCodeAt(0)) >>> 0;
  const colors = ['#A77DD8', '#3C9C8A', '#CD8B51', '#5C8BCC', '#C97388'];
  return { name, color: colors[hash % colors.length], path: '', monogram: name.trim().slice(0, 2).toUpperCase() || '?', known: false };
}

/** Relative luminance (0 dark .. 1 light) of a `#rrggbb` colour. */
export function hexLuminance(hex: string): number {
  const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
  if (!m) return 0.5;
  const ch = [0, 2, 4].map((i) => {
    const v = parseInt(m[1].slice(i, i + 2), 16) / 255;
    return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * ch[0] + 0.7152 * ch[1] + 0.0722 * ch[2];
}

/**
 * Colour of a bare logo (no tile behind it): the brand colour, except marks so
 * dark or so light that they would vanish on the theme surface, which follow
 * the text colour instead.
 */
export function bareBrandColor(hex: string): string {
  const l = hexLuminance(hex);
  return l < 0.06 || l > 0.9 ? 'currentColor' : hex;
}

import { siGithub, siGitlab, siNotion, siGoogledrive, siGooglecalendar, siGmail, siDiscord, siFigma, siLinear, siAnthropic, siGooglegemini, siOllama, siClaude, siPostgresql, siSqlite, siBrave, siPuppeteer, siSentry, siTodoist, siObsidian, siHomeassistant, siOpenrouter, siDeepseek, siMistralai, siCursor, siGithubcopilot, siLmstudio, siOpencode, siStripe, siAtlassian, siJira, siConfluence, siTrello, siAsana, siHubspot, siShopify, siDropbox, siZoom, siX, siFacebook, siInstagram, siYoutube, siReddit, siTelegram, siWhatsapp, siSpotify, siAirtable, siClickup, siZendesk, siIntercom, siMailchimp, siCalendly, siDocker, siVercel, siNetlify, siSupabase, siCloudflare, siGooglesheets, siGoogledocs, siGoogleslides, siGooglemeet, siGooglemaps, siGooglephotos, siBitbucket, siDatadog, siPagerduty, siMongodb, siRedis, siMiro, siBox, siEvernote, siPaypal, siSquare, siQuickbooks, siXero, siWordpress, siMedium, siGhost, siSubstack, siZapier, siMake, siN8n, siResend, siMixpanel, siPosthog, siFirebase, siDigitalocean, siQwen, siKimi, siTiktok, siNeon, siWebflow } from 'simple-icons';

const icons = [siGithub, siGitlab, siNotion, siGoogledrive, siGooglecalendar, siGmail, siDiscord, siFigma, siLinear, siAnthropic, siGooglegemini, siOllama, siClaude, siPostgresql, siSqlite, siBrave, siPuppeteer, siSentry, siTodoist, siObsidian, siHomeassistant, siOpenrouter, siDeepseek, siMistralai, siCursor, siGithubcopilot, siLmstudio, siOpencode, siStripe, siAtlassian, siJira, siConfluence, siTrello, siAsana, siHubspot, siShopify, siDropbox, siZoom, siX, siFacebook, siInstagram, siYoutube, siReddit, siTelegram, siWhatsapp, siSpotify, siAirtable, siClickup, siZendesk, siIntercom, siMailchimp, siCalendly, siDocker, siVercel, siNetlify, siSupabase, siCloudflare, siGooglesheets, siGoogledocs, siGoogleslides, siGooglemeet, siGooglemaps, siGooglephotos, siBitbucket, siDatadog, siPagerduty, siMongodb, siRedis, siMiro, siBox, siEvernote, siPaypal, siSquare, siQuickbooks, siXero, siWordpress, siMedium, siGhost, siSubstack, siZapier, siMake, siN8n, siResend, siMixpanel, siPosthog, siFirebase, siDigitalocean, siQwen, siKimi, siTiktok, siNeon, siWebflow];
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
  xtwitter: 'x',
  whatsappbusiness: 'whatsapp',
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

// Explicit entries, not invented logos: brands whose terms do not allow their logo in an app list.
// See docs/design/brand-icons.md for the reason and source of each. `known` means bundled, so AppLogo never fetches these.
const initials: Record<string, string> = {
  groq: 'Groq',
  together: 'Together AI',
  meta: 'Meta',
  playwright: 'Playwright'
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
  // Vendor-published marks for tools Praxi connects to, each reduced to one path in a 24-box (coordinates
  // rescaled, geometry unchanged); sources, terms and dates in docs/design/brand-icons.md, files in src/assets/brands/.
  if (key === 'llamacpp') return { name: 'llama.cpp', color: '#111111', path: 'M24 15.68L20.17 22.32L20.165 22.317C19.49 23.363 18.328 24 17.075 24H4.8L9.6 15.68H24ZM9.6 15.68H0L7.984 1.841C8.641 0.702 9.856 0 11.172 0H18.646L9.6 15.68Z', monogram: '', known: true, monochrome: true };
  if (key === 'tavily') return { name: 'Tavily', color: '#1F1E1E', path: 'M16.935 0C19.408 0 20.644 0 21.589 0.481C22.42 0.905 23.095 1.58 23.519 2.411C24 3.356 24 4.592 24 7.065V16.935C24 19.408 24 20.644 23.519 21.589C23.095 22.42 22.42 23.095 21.589 23.519C20.644 24 19.408 24 16.935 24H7.065C4.592 24 3.356 24 2.411 23.519C1.58 23.095 0.905 22.42 0.481 21.589C0 20.644 0 19.408 0 16.935V7.065C0 4.592 0 3.356 0.481 2.411C0.905 1.58 1.58 0.905 2.411 0.481C3.356 0 4.592 0 7.065 0H16.935ZM10.205 13.268C10.075 13.268 9.946 13.293 9.826 13.343C9.706 13.393 9.597 13.466 9.505 13.559L7.616 15.455L7.114 14.951C6.745 14.581 6.113 14.764 5.999 15.276L5.283 18.459C5.258 18.569 5.261 18.683 5.292 18.791C5.323 18.898 5.381 18.996 5.46 19.075L5.459 19.076C5.538 19.156 5.636 19.214 5.743 19.245C5.851 19.276 5.965 19.28 6.074 19.254L9.245 18.536C9.754 18.421 9.937 17.787 9.568 17.416L9.066 16.912L10.955 15.016C11.141 14.829 11.245 14.577 11.245 14.313C11.245 14.05 11.141 13.797 10.955 13.611L10.93 13.585L10.931 13.585L10.905 13.559C10.813 13.466 10.704 13.393 10.584 13.343C10.464 13.293 10.335 13.268 10.205 13.268ZM17.021 12.073C16.579 11.793 16.004 12.111 16.004 12.635V13.348H11.956C12.091 13.607 12.168 13.902 12.168 14.216C12.168 14.668 12.008 15.084 11.742 15.408L16.004 15.408L16.004 16.12C16.004 16.644 16.579 16.963 17.021 16.683L19.769 14.941C19.975 14.809 20.078 14.593 20.078 14.377C20.077 14.162 19.975 13.946 19.769 13.815L17.021 12.073ZM10.303 4.46C10.191 4.459 10.08 4.488 9.982 4.542C9.884 4.596 9.801 4.675 9.742 4.77L8.006 7.529H8.005C7.727 7.972 8.044 8.55 8.566 8.55H9.276V12.649C9.571 12.454 9.923 12.34 10.302 12.34C10.681 12.34 11.034 12.454 11.329 12.649V8.55H12.039C12.561 8.55 12.878 7.972 12.599 7.529L10.863 4.77C10.732 4.564 10.518 4.46 10.303 4.46Z', monogram: '', known: true, monochrome: true };
  if (key === 'context7') return { name: 'Context7', color: '#292524', path: 'M7.315 13.718C7.315 16.788 6.069 19.351 4.042 21.712H8.763L8.763 24H1.522V21.828C3.738 19.322 4.592 17.28 4.592 13.718L7.315 13.718ZM16.685 13.718C16.685 16.788 17.931 19.351 19.958 21.712H15.237V24H22.478V21.828C20.262 19.322 19.408 17.28 19.408 13.718H16.685ZM7.315 10.282C7.315 7.212 6.069 4.649 4.042 2.288L8.763 2.288V0L1.522 0V2.172C3.738 4.678 4.592 6.72 4.592 10.282L7.315 10.282ZM16.685 10.282C16.685 7.212 17.931 4.649 19.958 2.288L15.237 2.288L15.237 0L22.478 0V2.172C20.262 4.678 19.408 6.72 19.408 10.282H16.685Z', monogram: '', known: true, monochrome: true };
  if (key === 'firecrawl') return { name: 'Firecrawl', color: '#FA5D19', path: 'M17.622 7.695C16.693 7.971 15.992 8.595 15.479 9.272C15.369 9.418 15.14 9.308 15.183 9.13C16.165 5.092 14.868 1.736 10.824 0.084C10.619 0 10.406 0.184 10.459 0.399C12.299 7.785 4.562 7.162 5.539 15.535C5.556 15.679 5.395 15.777 5.277 15.693C4.911 15.43 4.501 14.881 4.22 14.495C4.138 14.381 3.96 14.413 3.922 14.549C3.698 15.357 3.592 16.119 3.592 16.876C3.592 19.818 5.105 22.409 7.394 23.91C7.525 23.996 7.693 23.873 7.649 23.723C7.531 23.327 7.465 22.91 7.46 22.479C7.46 22.214 7.476 21.944 7.518 21.691C7.614 21.057 7.834 20.454 8.204 19.904C9.472 18 12.016 16.16 11.609 13.662C11.583 13.504 11.77 13.4 11.888 13.509C13.679 15.146 14.034 17.347 13.74 19.322C13.715 19.494 13.93 19.585 14.038 19.451C14.312 19.108 14.647 18.807 15.011 18.581C15.102 18.524 15.223 18.567 15.257 18.668C15.46 19.257 15.761 19.811 16.045 20.364C16.385 21.029 16.566 21.788 16.537 22.592C16.523 22.983 16.458 23.361 16.349 23.721C16.303 23.873 16.47 24 16.603 23.913C18.894 22.411 20.408 19.821 20.408 16.877C20.408 15.853 20.229 14.85 19.89 13.911C19.179 11.939 17.377 10.459 17.833 7.889C17.854 7.767 17.741 7.66 17.622 7.695Z', monogram: '', known: true };
  if (key === 'exa') return { name: 'Exa', color: '#0143D9', path: 'M21.923 1.86C21.923 1.89 21.913 1.92 21.894 1.943L13.744 12L21.894 22.057C21.913 22.08 21.923 22.11 21.923 22.14V23.868C21.923 23.941 21.864 24 21.791 24H2.209C2.136 24 2.077 23.941 2.077 23.868V0.132C2.077 0.059 2.136 0 2.209 0H21.791C21.864 0 21.923 0.059 21.923 0.132V1.86ZM6.086 21.457H18.129L12.108 14.021L6.086 21.457ZM4.621 13.221V19.225L9.482 13.221H4.621ZM4.621 10.679H9.401L4.621 4.775V10.679ZM12.108 9.979L18.129 2.543H6.086L12.108 9.979Z', monogram: '', known: true };
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

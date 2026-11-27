# Her uygulamayı bağlamak için ne gerekir

Uygulamalar listesindeki her uygulama, özenle seçilmiş bir katalogdan gelir. Bu sayfa, her biri
için **oturum açmanın neye mal olduğunu**, sunucunun nerede çalıştığını ve yazılımının hangi
lisansla yayımlandığını söyler. Burada hiçbir şey kendiliğinden bağlanmaz: bir uygulama, sen
**Uygulamalar**'ı açıp onu seçerek bağlayana kadar kapalı kalır; sunduğu her araç da bir şeyi
değiştirmeden önce yine sorar ([Uygulamaları bağlama](apps.md) sayfasına bak).

## Oturum açmanın dört yolu

| Ne gerekir | Anlamı |
| --- | --- |
| Ayar gerekmez | Hesap da anahtar da yok: yerel bir araç ya da herkese açık bir sunucu. |
| Tarayıcıda hesabınla oturum aç | Her zamanki tek tıklık yol. Tarayıcın sağlayıcının giriş sayfasını açar; Praxi kendini sağlayıcıya kendisi tanıtır (dinamik istemci kaydı), yani önceden bir şey oluşturmazsın. |
| Kendi oluşturduğun bir OAuth istemcisiyle oturum aç | Sağlayıcı, önce kendi hesabında kaydettiğin bir uygulama ister (Google: kendi Cloud projen). Praxi seni adım adım yönlendirir; [Google Workspace](google-workspace.md) sayfasına bak. |
| Bir API anahtarı, erişim anahtarı ya da bağlantı adresi yapıştır | Anahtarı sağlayıcıda oluşturur ve bir kez yapıştırırsın. Gizli bilgi deposunda saklanır, bir daha gösterilmez. |

**Nerede çalışır.** *Sağlayıcının sunucusu*, sunucuyu sağlayıcının çalıştırdığı ve Praxi'nin
onu yalnızca internet üzerinden çağırdığı anlamına gelir; bilgisayarına bir şey kurulmaz ve
sağlayıcının kendi koşulları geçerlidir (lisans sütununda *Sağlayıcı barındırır* yazar). *Bu
bilgisayar* ise Praxi'nin makinende, tek bir sürüme sabitlenmiş bir paketi başlattığı
anlamına gelir; lisans sütunu o paketin lisansıdır.

## Katalog

<!-- BEGIN:app-catalog-table -->
| Uygulama | Ne gerekir | Nerede çalışır | Sunucu lisansı |
| --- | --- | --- | --- |
| [Airtable](https://airtable.com/developers/agents/mcp/getting-started) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Atlassian](https://www.atlassian.com/platform/remote-mcp-server) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Brave Search](https://github.com/brave/brave-search-mcp-server) | Bir API anahtarı, erişim anahtarı ya da bağlantı adresi yapıştır | Bu bilgisayar | MIT |
| [Calendly](https://developer.calendly.com/docs/mcp/calendly-mcp-server) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [ClickUp](https://developer.clickup.com/docs/connect-an-ai-assistant-to-clickups-mcp-server) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Cloudflare](https://developers.cloudflare.com/agents/model-context-protocol/cloudflare/servers-for-cloudflare/) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Cloudflare Docs](https://github.com/cloudflare/mcp-server-cloudflare/tree/main/apps/docs-ai-search) | Ayar gerekmez | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Context7](https://github.com/upstash/context7) | Ayar gerekmez | Bu bilgisayar | MIT |
| [DeepWiki](https://docs.devin.ai/work-with-devin/deepwiki-mcp) | Ayar gerekmez | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Dropbox](https://help.dropbox.com/integrations/connect-dropbox-mcp-server) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Exa](https://github.com/exa-labs/exa-mcp-server) | Bir API anahtarı, erişim anahtarı ya da bağlantı adresi yapıştır | Bu bilgisayar | MIT |
| [Fetch](https://github.com/modelcontextprotocol/servers/tree/main/src/fetch) | Ayar gerekmez | Bu bilgisayar | MIT |
| [Figma](https://developers.figma.com/docs/figma-mcp-server/) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Filesystem](https://github.com/modelcontextprotocol/servers/tree/main/src/filesystem) | Ayar gerekmez | Bu bilgisayar | Apache-2.0 AND MIT |
| [Firecrawl](https://github.com/firecrawl/firecrawl-mcp-server) | Bir API anahtarı, erişim anahtarı ya da bağlantı adresi yapıştır | Bu bilgisayar | MIT |
| [GitHub (access token)](https://github.com/github/github-mcp-server) | Bir API anahtarı, erişim anahtarı ya da bağlantı adresi yapıştır | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [GitLab](https://docs.gitlab.com/user/model_context_protocol/mcp_server/) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Gmail (your own Google client)](https://developers.google.com/workspace/guides/configure-mcp-servers) | Kendi oluşturduğun bir OAuth istemcisiyle oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Google Calendar (your own Google client)](https://developers.google.com/workspace/guides/configure-mcp-servers) | Kendi oluşturduğun bir OAuth istemcisiyle oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Google Drive (your own Google client)](https://developers.google.com/workspace/guides/configure-mcp-servers) | Kendi oluşturduğun bir OAuth istemcisiyle oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Home Assistant](https://github.com/voska/hass-mcp) | Bir API anahtarı, erişim anahtarı ya da bağlantı adresi yapıştır | Bu bilgisayar | MIT |
| [Intercom](https://developers.intercom.com/docs/guides/mcp) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Linear](https://linear.app/docs/mcp) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Microsoft Learn](https://github.com/microsoftdocs/mcp) | Ayar gerekmez | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Miro](https://developers.miro.com/docs/miro-mcp) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [monday.com](https://developer.monday.com/api-reference/docs/integrate-with-monday-mcp) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [MongoDB](https://github.com/mongodb-js/mongodb-mcp-server) | Bir API anahtarı, erişim anahtarı ya da bağlantı adresi yapıştır | Bu bilgisayar | Apache-2.0 |
| [Neon](https://neon.com/docs/ai/neon-mcp-server) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Netlify](https://github.com/netlify/netlify-mcp) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Notion](https://github.com/makenotion/notion-mcp-server) | Bir API anahtarı, erişim anahtarı ya da bağlantı adresi yapıştır | Bu bilgisayar | MIT |
| [Notion](https://developers.notion.com/docs/mcp) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Obsidian](https://github.com/MarkusPfundstein/mcp-obsidian) | Bir API anahtarı, erişim anahtarı ya da bağlantı adresi yapıştır | Bu bilgisayar | MIT |
| [PayPal](https://developer.paypal.com/ai-tools/mcp-server) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Playwright](https://github.com/microsoft/playwright-mcp) | Ayar gerekmez | Bu bilgisayar | Apache-2.0 |
| [PostgreSQL](https://github.com/crystaldba/postgres-mcp) | Bir API anahtarı, erişim anahtarı ya da bağlantı adresi yapıştır | Bu bilgisayar | MIT |
| [PostHog](https://posthog.com/docs/model-context-protocol) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Sentry](https://github.com/getsentry/sentry-mcp) | Bir API anahtarı, erişim anahtarı ya da bağlantı adresi yapıştır | Bu bilgisayar | FSL-1.1-ALv2 |
| [Slack](https://github.com/modelcontextprotocol/servers-archived/tree/main/src/slack) | Bir API anahtarı, erişim anahtarı ya da bağlantı adresi yapıştır | Bu bilgisayar | MIT |
| [SQLite](https://github.com/modelcontextprotocol/servers-archived/tree/main/src/sqlite) | Ayar gerekmez | Bu bilgisayar | MIT |
| [Square](https://developer.squareup.com/docs/mcp) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Stripe](https://docs.stripe.com/mcp) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Supabase](https://supabase.com/docs/guides/ai-tools/mcp) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Tavily](https://github.com/tavily-ai/tavily-mcp) | Bir API anahtarı, erişim anahtarı ya da bağlantı adresi yapıştır | Bu bilgisayar | MIT |
| [Todoist](https://github.com/todoist/todoist-mcp) | Bir API anahtarı, erişim anahtarı ya da bağlantı adresi yapıştır | Bu bilgisayar | MIT |
| [Vercel](https://vercel.com/docs/agent-resources/vercel-mcp) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
| [Webflow](https://developers.webflow.com/mcp/reference/overview) | Tarayıcıda hesabınla oturum aç | Sağlayıcının sunucusu | Sağlayıcı barındırır |
<!-- END:app-catalog-table -->

Tablo `crates/mcp-bridge/catalog.json` dosyasından üretilir; ikisi ayrışırsa bir denetim başarısız
olur. Kataloğa yalnızca resmî sunucular (sağlayıcının kendisinden ya da MCP kayıt defterinde
listelenmiş olanlar) eklenir. Bazı sağlayıcıların sunucusu yalnızca önceden kendilerine kaydettiğin
bir istemciyi kabul eder (bu yazıldığında: Asana, HubSpot ve Box); kurulumsuz oturum açmak
mümkün olana kadar bunlar listede yer almaz.

## İlgili sayfalar

- [Uygulamaları bağlama](apps.md): bağlama, yönetme ve oturumu kapatma.
- [MCP sunucuları](mcp-servers.md): sunucuyu elle eklemek.
- [Botunu güvende tut](keep-your-bot-safe.md): izin düzeylerinin anlamı.

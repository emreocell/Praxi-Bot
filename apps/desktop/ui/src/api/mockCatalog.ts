import type { McpCatalogEntry } from "./types.ts";

/**
 * Stand-in for `GET /v1/mcp/catalog` used by the mock client (design review,
 * screenshots and tests).  The real list is curated by the daemon.
 */
export const MOCK_MCP_CATALOG: McpCatalogEntry[] = [
  {
    "id": "filesystem",
    "name": "Filesystem",
    "description": "Read and write files in folders you choose.",
    "category": "files",
    "homepage": "https://github.com/modelcontextprotocol/servers",
    "icon": "files",
    "install": {
      "transport": "stdio",
      "command": "npx",
      "args": [
        "-y",
        "@modelcontextprotocol/server-filesystem",
        "."
      ],
      "required_secrets": []
    }
  },
  {
    "id": "github",
    "name": "GitHub",
    "description": "Repos, issues, pull requests and code search.",
    "category": "developer",
    "homepage": "https://github.com/github/github-mcp-server",
    "icon": "github",
    "install": {
      "transport": "stdio",
      "command": "npx",
      "args": [
        "-y",
        "@modelcontextprotocol/server-github"
      ],
      "required_secrets": [
        {
          "name": "GITHUB_PERSONAL_ACCESS_TOKEN",
          "label": "GitHub personal access token",
          "help_url": "https://github.com/settings/tokens"
        }
      ]
    }
  },
  {
    "id": "gitlab",
    "name": "GitLab",
    "description": "Projects, merge requests and pipelines.",
    "category": "developer",
    "homepage": "https://github.com/modelcontextprotocol/servers",
    "icon": "gitlab",
    "install": {
      "transport": "stdio",
      "command": "npx",
      "args": [
        "-y",
        "@modelcontextprotocol/server-gitlab"
      ],
      "required_secrets": [
        {
          "name": "GITLAB_PERSONAL_ACCESS_TOKEN",
          "label": "GitLab access token",
          "help_url": "https://gitlab.com/-/user_settings/personal_access_tokens"
        }
      ]
    }
  },
  {
    "id": "slack",
    "name": "Slack",
    "description": "Read channels and post messages.",
    "category": "communication",
    "homepage": "https://github.com/modelcontextprotocol/servers",
    "icon": "slack",
    "install": {
      "transport": "stdio",
      "command": "npx",
      "args": [
        "-y",
        "@modelcontextprotocol/server-slack"
      ],
      "required_secrets": [
        {
          "name": "SLACK_BOT_TOKEN",
          "label": "Slack bot token",
          "help_url": "https://api.slack.com/apps"
        }
      ]
    }
  },
  {
    "id": "notion",
    "name": "Notion",
    "description": "Search and edit pages and databases.",
    "category": "productivity",
    "homepage": "https://github.com/makenotion/notion-mcp-server",
    "icon": "notion",
    "install": {
      "transport": "stdio",
      "command": "npx",
      "args": [
        "-y",
        "@notionhq/notion-mcp-server"
      ],
      "required_secrets": [
        {
          "name": "NOTION_TOKEN",
          "label": "Notion integration token",
          "help_url": "https://www.notion.so/profile/integrations"
        }
      ]
    }
  },
  {
    "id": "google-drive",
    "name": "Google Drive",
    "description": "Find and read files in Drive.",
    "category": "files",
    "homepage": "https://github.com/modelcontextprotocol/servers",
    "icon": "googledrive",
    "install": {
      "transport": "stdio",
      "command": "npx",
      "args": [
        "-y",
        "@modelcontextprotocol/server-gdrive"
      ],
      "required_secrets": [
        {
          "name": "GDRIVE_CREDENTIALS",
          "label": "Google OAuth credentials JSON",
          "help_url": "https://console.cloud.google.com/apis/credentials"
        }
      ]
    }
  },
  {
    "id": "postgres",
    "name": "PostgreSQL",
    "description": "Run read-only queries on a Postgres database.",
    "category": "data",
    "homepage": "https://github.com/modelcontextprotocol/servers",
    "icon": "postgresql",
    "install": {
      "transport": "stdio",
      "command": "npx",
      "args": [
        "-y",
        "@modelcontextprotocol/server-postgres"
      ],
      "required_secrets": [
        {
          "name": "POSTGRES_URL",
          "label": "Connection string"
        }
      ]
    }
  },
  {
    "id": "sqlite",
    "name": "SQLite",
    "description": "Query a local SQLite database file.",
    "category": "data",
    "homepage": "https://github.com/modelcontextprotocol/servers",
    "icon": "sqlite",
    "install": {
      "transport": "stdio",
      "command": "uvx",
      "args": [
        "mcp-server-sqlite"
      ],
      "required_secrets": []
    }
  },
  {
    "id": "brave-search",
    "name": "Brave Search",
    "description": "Web and local search.",
    "category": "search",
    "homepage": "https://github.com/modelcontextprotocol/servers",
    "icon": "brave",
    "install": {
      "transport": "stdio",
      "command": "npx",
      "args": [
        "-y",
        "@modelcontextprotocol/server-brave-search"
      ],
      "required_secrets": [
        {
          "name": "BRAVE_API_KEY",
          "label": "Brave Search API key",
          "help_url": "https://brave.com/search/api/"
        }
      ]
    }
  },
  {
    "id": "fetch",
    "name": "Fetch",
    "description": "Download a web page and turn it into text.",
    "category": "search",
    "homepage": "https://github.com/modelcontextprotocol/servers",
    "icon": null,
    "install": {
      "transport": "stdio",
      "command": "uvx",
      "args": [
        "mcp-server-fetch"
      ],
      "required_secrets": []
    }
  },
  {
    "id": "playwright",
    "name": "Playwright",
    "description": "Drive a real browser: click, type, read pages.",
    "category": "browser",
    "homepage": "https://github.com/microsoft/playwright-mcp",
    "icon": "playwright",
    "install": {
      "transport": "stdio",
      "command": "npx",
      "args": [
        "-y",
        "@playwright/mcp@latest"
      ],
      "required_secrets": []
    }
  },
  {
    "id": "puppeteer",
    "name": "Puppeteer",
    "description": "Browser automation and screenshots.",
    "category": "browser",
    "homepage": "https://github.com/modelcontextprotocol/servers",
    "icon": "puppeteer",
    "install": {
      "transport": "stdio",
      "command": "npx",
      "args": [
        "-y",
        "@modelcontextprotocol/server-puppeteer"
      ],
      "required_secrets": []
    }
  },
  {
    "id": "sentry",
    "name": "Sentry",
    "description": "Look into errors and issues.",
    "category": "developer",
    "homepage": "https://github.com/getsentry/sentry-mcp",
    "icon": "sentry",
    "install": {
      "transport": "http",
      "url": "https://mcp.sentry.dev/mcp",
      "required_secrets": [
        {
          "name": "SENTRY_ACCESS_TOKEN",
          "label": "Sentry auth token",
          "help_url": "https://sentry.io/settings/auth-tokens/"
        }
      ]
    }
  },
  {
    "id": "linear",
    "name": "Linear",
    "description": "Issues, projects and cycles.",
    "category": "productivity",
    "homepage": "https://linear.app/docs/mcp",
    "icon": "linear",
    "install": {
      "transport": "http",
      "url": "https://mcp.linear.app/mcp",
      "required_secrets": [
        {
          "name": "LINEAR_API_KEY",
          "label": "Linear API key",
          "help_url": "https://linear.app/settings/api"
        }
      ]
    }
  },
  {
    "id": "todoist",
    "name": "Todoist",
    "description": "Tasks and projects.",
    "category": "productivity",
    "homepage": "https://github.com/doist/todoist-ai",
    "icon": "todoist",
    "install": {
      "transport": "stdio",
      "command": "npx",
      "args": [
        "-y",
        "@doist/todoist-ai"
      ],
      "required_secrets": [
        {
          "name": "TODOIST_API_KEY",
          "label": "Todoist API token",
          "help_url": "https://app.todoist.com/app/settings/integrations/developer"
        }
      ]
    }
  },
  {
    "id": "obsidian",
    "name": "Obsidian",
    "description": "Search and edit notes in a vault.",
    "category": "productivity",
    "homepage": "https://github.com/MarkusPfundstein/mcp-obsidian",
    "icon": "obsidian",
    "install": {
      "transport": "stdio",
      "command": "uvx",
      "args": [
        "mcp-obsidian"
      ],
      "required_secrets": [
        {
          "name": "OBSIDIAN_API_KEY",
          "label": "Obsidian Local REST API key"
        }
      ]
    }
  },
  {
    "id": "home-assistant",
    "name": "Home Assistant",
    "description": "Control lights, switches and sensors.",
    "category": "home",
    "homepage": "https://www.home-assistant.io/integrations/mcp_server/",
    "icon": "homeassistant",
    "install": {
      "transport": "http",
      "url": "http://homeassistant.local:8123/mcp_server/sse",
      "required_secrets": [
        {
          "name": "HASS_TOKEN",
          "label": "Long-lived access token",
          "help_url": "https://www.home-assistant.io/docs/authentication/#your-account-profile"
        }
      ]
    }
  },
  {
    "id": "exa",
    "name": "Exa",
    "description": "Neural web search.",
    "category": "search",
    "homepage": "https://github.com/exa-labs/exa-mcp-server",
    "icon": null,
    "install": {
      "transport": "http",
      "url": "https://mcp.exa.ai/mcp",
      "required_secrets": [
        {
          "name": "EXA_API_KEY",
          "label": "Exa API key",
          "help_url": "https://dashboard.exa.ai/api-keys"
        }
      ]
    }
  },
  {
    "id": "tavily",
    "name": "Tavily",
    "description": "Search built for agents.",
    "category": "search",
    "homepage": "https://github.com/tavily-ai/tavily-mcp",
    "icon": null,
    "install": {
      "transport": "stdio",
      "command": "npx",
      "args": [
        "-y",
        "tavily-mcp@latest"
      ],
      "required_secrets": [
        {
          "name": "TAVILY_API_KEY",
          "label": "Tavily API key",
          "help_url": "https://app.tavily.com/home"
        }
      ]
    }
  },
  {
    "id": "firecrawl",
    "name": "Firecrawl",
    "description": "Crawl and scrape sites into clean text.",
    "category": "search",
    "homepage": "https://github.com/mendableai/firecrawl-mcp-server",
    "icon": null,
    "install": {
      "transport": "stdio",
      "command": "npx",
      "args": [
        "-y",
        "firecrawl-mcp"
      ],
      "required_secrets": [
        {
          "name": "FIRECRAWL_API_KEY",
          "label": "Firecrawl API key",
          "help_url": "https://www.firecrawl.dev/app/api-keys"
        }
      ]
    }
  },
  {
    "id": "context7",
    "name": "Context7",
    "description": "Up-to-date library documentation.",
    "category": "developer",
    "homepage": "https://github.com/upstash/context7",
    "icon": null,
    "install": {
      "transport": "stdio",
      "command": "npx",
      "args": [
        "-y",
        "@upstash/context7-mcp"
      ],
      "required_secrets": []
    }
  }
];

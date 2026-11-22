# Telegram

Sprich von deinem Handy aus mit deinen Bots. Der Daemon betreibt einen Telegram-Kanal: Du schreibst deinem Bot,
siehst die Antwort live eintreffen und genehmigst oder lehnst Aktionen, die eine Freigabe brauchen, mit
Schaltflächen im Chat ab. Nur Telegram-Konten, die du **koppelst**, dürfen ihn nutzen; alle anderen werden abgewiesen.

Die vollständige Einrichtungsanleitung steht in [docs/telegram.md](../../telegram.md) (auf Englisch). Kurz gesagt:

```sh
bot channels telegram setup     # das Token von @BotFather einfügen (verdeckte Eingabe)
bot serve                       # der Kanal startet zusammen mit dem Daemon
bot channels telegram pair      # gibt einen einmaligen 6-stelligen Code aus (10 Minuten gültig)
```

Öffne dann deinen Bot in Telegram und sende `/pair 123456` (mit deinem Code).

## Im Chat

| Nachricht | Wirkung |
| --- | --- |
| `/bots` | Listet deine Bots auf |
| `/use <Botname oder ID>` | Wählt den Bot, mit dem dieser Chat spricht |
| `/stop` | Stoppt den aktuellen Lauf |
| `/status` | Zeigt den Status |
| `/lang tr` oder `/lang en` | Stellt die Sprache dieses Chats auf Türkisch oder Englisch (`/lang` allein zeigt die aktuelle) |
| alles andere | Wird als Nachricht an den gewählten Bot gesendet |

Braucht ein Bot eine Genehmigung oder stellt er eine Frage, bekommst du eine Nachricht mit Schaltflächen. Der Lauf
wartet, bis du eine antippst (oder, bei geplanten Läufen, bis das Zeitlimit von 30 Minuten ihn pausiert).

## Sprache

Befehle, Genehmigungskarten, Schaltflächen und Hinweise des Kanals selbst sind auf Englisch oder Türkisch.
Jeder Chat hat seine eigene Sprache und behält sie über Neustarts hinweg; `/lang` ändert sie jederzeit.
Was deine Bots schreiben, Werkzeugausgaben und Daemon-Meldungen werden nicht übersetzt.

- Ein Chat, der **vor** Einführung der Chat-Sprachen gekoppelt wurde, oder ein in der Konfiguration eingetragener
  Nutzer bleibt Englisch, bis jemand in diesem Chat `/lang` sendet. Ein Update ändert die Sprache einer
  Genehmigungskarte nie von selbst.
- Ein Chat, der ab jetzt mit `/pair` gekoppelt wird, startet je nach Sprache der Telegram-App auf Türkisch oder
  Englisch; jede andere App-Sprache fällt auf Englisch zurück. (Der Kanal kann auch auf die Sprache der
  Desktop-App zurückfallen, aber die Desktop-App gibt sie noch nicht an den Daemon weiter.)
- Nur die Beschriftungen der Schaltflächen ändern sich mit der Sprache. Was eine Schaltfläche tut, die
  Informationen auf der Karte und was maskiert wird, bleiben gleich.
- Einige feste Texte (Antworten bei der Kopplung, Hinweise zum Ende eines Laufs) sind noch nur auf Englisch.

## Gut zu wissen

- Erstelle deinen eigenen Telegram-Bot mit BotFather; das Token gehört dir. Praxi fragt Telegram von deinem Rechner
  aus ab, daher ist kein eingehender Port und keine öffentliche Adresse nötig.
- Falsche Kopplungscodes werden abgewiesen, und nach einigen falschen Versuchen ist die Kopplung eine Weile
  gesperrt. Nachrichten von nicht gekoppelten Konten werden mit einem festen Text abgewiesen und in der Rate begrenzt.
- Alles, was an Telegram gesendet wird, läuft durch den Geheimnis-Schwärzer, der bekannte geheime Werte
  (auch dein Bot-Token) maskiert.
- `bot channels telegram status` zeigt, ob der Kanal eingerichtet, aktiviert und aktiv ist und wie viele Nutzer
  gekoppelt sind. `bot channels telegram
  disable` schaltet ihn beim nächsten `bot serve` ab; das Token bleibt gespeichert.
- Von Telegram gestartete Läufe sind ganz normale Läufe des gewählten Bots und folgen den Berechtigungsregeln
  dieses Bots wie Läufe, die du am Schreibtisch startest. (Ein eigener Auslöser `channel` existiert im
  Richtlinienmodell, wird Telegram-Läufen aber noch nicht zugewiesen.)
- Telegram-Nachrichten laufen über die Server von Telegram. Nutze es nicht für etwas, das du nicht auch über
  Telegram senden würdest.

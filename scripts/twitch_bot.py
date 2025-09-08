import asyncio
from twitchio.ext import commands

class TwitchBot(commands.Bot):
    def __init__(self, token, client_id, client_secret, bot_username, bot_id, channel, mention_callback):
        self.bot_username = bot_username.lower()
        self.channel_name = channel.lower()
        self.mention_callback = mention_callback
        super().__init__(
            token=token,
            client_id=client_id,
            client_secret=client_secret,
            nick=bot_username,
            prefix='!',
            initial_channels=[self.channel_name],
            bot_id=bot_id
        )

    async def event_ready(self):
        """Called once when the bot goes online."""
        print(f'{self.nick} is online!')
        ws = self.ws  # this is the websocket
        await ws.send_privmsg(self.channel_name, f"/me has landed!")

    async def event_message(self, message):
        """Runs every time a message is sent in chat."""
        # make sure the bot ignores itself
        if message.author.name.lower() == self.nick.lower():
            return

        # Check for mentions
        if message.content.lower().startswith(f'@{self.bot_username}'):
            print(f"Mention received from {message.author.name}: {message.content}")
            # Remove the mention part to get the actual prompt
            prompt = message.content[len(f'@{self.bot_username}'):].strip()
            if self.mention_callback:
                # We need to run the callback in a way that doesn't block the bot's event loop.
                # If the callback is a regular function, we can run it in an executor.
                # If it's an async function, we can await it.
                # For now, let's assume it's a regular function.
                loop = asyncio.get_running_loop()
                loop.run_in_executor(None, self.mention_callback, message.author.name, prompt)

        await self.handle_commands(message)

    async def send_chat_message(self, message):
        """Sends a message to the chat."""
        channel = self.get_channel(self.channel_name)
        if channel:
            await channel.send(message)
        else:
            print(f"Error: Could not find channel {self.channel_name}")

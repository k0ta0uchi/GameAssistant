import asyncio
from twitchio.ext import commands

class TwitchBot(commands.Bot):
    def __init__(self, token, client_id, client_secret, bot_username, channel, mention_callback):
        self.bot_username = bot_username.lower()
        self.channel_name = channel.lower()
        self.mention_callback = mention_callback
        # Per documentation and errors, the modern twitchio Bot requires token and client_id/secret.
        # We will not use bot_id as it seems to be a red herring. The library can derive it.
        # The channel to join is passed in initial_channels.
        super().__init__(
            token=token,
            client_id=client_id,
            client_secret=client_secret,
            prefix='!', # A prefix is required, but we don't use commands.
            initial_channels=[self.channel_name]
        )

    async def event_ready(self):
        """Called once when the bot goes online."""
        print(f"'{self.nick}' is online!") # self.nick should be available now.
        print(f"Attempting to send connection message to '{self.channel_name}'")
        channel = await self.fetch_channel(self.channel_name)
        if channel:
            await channel.send(f"/me has landed!")
            print(f"Successfully sent connection message to '{self.channel_name}'")
        else:
            print(f"Error: Could not find channel '{self.channel_name}' in event_ready.")

    async def event_message(self, message):
        """Runs every time a message is sent in chat."""
        # Make sure the bot ignores itself and empty messages
        if message.author is None or message.author.name.lower() == self.nick.lower():
            return

        # The bot's nick is automatically populated by twitchio on connection.
        # Let's handle commands first, as is good practice.
        await self.handle_commands(message)

        # Check for mentions
        if message.content.lower().startswith(f'@{self.nick.lower()}'):
            print(f"Mention received from {message.author.name}: {message.content}")
            prompt = message.content[len(f'@{self.nick.lower()}'):].strip()
            if self.mention_callback:
                # Run the synchronous callback in a thread to avoid blocking the bot's event loop
                loop = asyncio.get_running_loop()
                loop.run_in_executor(None, self.mention_callback, message.author.name, prompt)

    async def send_chat_message(self, message):
        """A method to be called from outside the bot's event loop to send a message."""
        print(f"Attempting to send message: '{message}' to '{self.channel_name}'")
        channel = await self.fetch_channel(self.channel_name)
        if channel:
            await channel.send(message)
            print("Message sent successfully.")
        else:
            print(f"Error: Could not find channel '{self.channel_name}' to send message.")

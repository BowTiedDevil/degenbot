export const InjectEnvPlugin = async () => {
  return {
    "shell.env": async (input, output) => {
      output.env.DEGENBOT_DEBUG = "true"
      output.env.DEGENBOT_PROGRESS_BAR = "false"
      output.env.DEGENBOT_ONE_CHUNK = "true"
    },
  }
}

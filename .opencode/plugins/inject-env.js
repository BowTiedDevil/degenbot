export const InjectEnvPlugin = async () => {
  return {
    "shell.env": async (input, output) => {
      output.env.DEGENBOT_PROGRESS_BAR = "false"
    },
  }
}

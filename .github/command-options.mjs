// Private CLI schema shared only by this repository's policy/report commands.
export function commandOptions(args, required) {
  if (args.length % 2) throw new Error("expected complete option/value pairs");
  const options = {};
  for (let i = 0; i < args.length; i += 2) {
    const [key, value] = args.slice(i, i + 2);
    if (!required.includes(key)) throw new Error(`unknown option: ${key}`);
    if (key in options) throw new Error(`duplicate option: ${key}`);
    if (!value || value.startsWith("--"))
      throw new Error(`missing value: ${key}`);
    options[key] = value;
  }
  for (const key of required)
    if (!(key in options)) throw new Error(`missing required option: ${key}`);
  return options;
}

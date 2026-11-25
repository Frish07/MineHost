export type Software='paper'|'purpur'|'vanilla'|'fabric'|'forge'|'neoforge';
export type Page='overview'|'settings'|'server'|'monitoring'|'console'|'software'|'mods'|'plugins'|'modpacks'|'files'|'worlds'|'players'|'properties'|'backups'|'network'|'playit'|'delete';
export function addonKind(software:string):'mods'|'plugins'|null{
 if(['paper','purpur'].includes(software))return 'plugins';
 if(['fabric','forge','neoforge'].includes(software))return 'mods';
 return null;
}
export function serverPages(software:string):Page[]{
 const kind=addonKind(software);
 return ['server','console','monitoring','software',...(kind?[kind]:[]),...(kind==='mods'?['modpacks' as const]:[]),'worlds','players','files','properties','backups','network','playit','delete'];
}

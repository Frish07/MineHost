switch ($r.action) {
  'adapters' {
    $items = @(Get-NetIPConfiguration -ErrorAction Stop | Where-Object { $_.NetAdapter.Status -eq 'Up' } | ForEach-Object {
      [pscustomobject]@{ name=$_.InterfaceAlias; ipv4=@($_.IPv4Address | ForEach-Object IPAddress); ipv6=@($_.IPv6Address | ForEach-Object IPAddress); gateway=@($_.IPv4DefaultGateway | ForEach-Object NextHop); profile=[string]$_.NetProfile.NetworkCategory }
    })
    ConvertTo-Json -InputObject $items -Depth 5 -Compress
  }
  'dns' {
    $records = @(); $errors = @()
    foreach ($type in @('A','AAAA','SRV')) {
      $name = if ($type -eq 'SRV') { '_minecraft._tcp.' + $r.domain } else { $r.target }
      try {
        $answer = @(Resolve-DnsName -Name $name -Type $type -DnsOnly -NoHostsFile -QuickTimeout -ErrorAction Stop)
        foreach ($entry in $answer) {
          if ([string]$entry.Type -eq $type) {
            $records += [pscustomobject]@{ name=$entry.Name; type=$type; address=[string]$entry.IPAddress; target=[string]$entry.NameTarget; port=[int]$entry.Port; priority=[int]$entry.Priority; weight=[int]$entry.Weight }
          }
        }
      } catch { $errors += ($name + ' / ' + $type + ': ' + $_.Exception.Message) }
    }
    ConvertTo-Json -InputObject @{records=@($records);errors=@($errors)} -Depth 6 -Compress
  }
  { $_ -in @('firewall_get','firewall_add','firewall_remove') } {
    if ($r.id -notmatch '^[a-fA-F0-9-]{36}$' -or [int]$r.port -lt 1024 -or [int]$r.port -gt 65535) { throw 'Invalid profile or port' }
    $name = 'MineHost-' + $r.id
    $rule = Get-NetFirewallRule -PolicyStore PersistentStore -ErrorAction Stop | Where-Object Name -eq $name | Select-Object -First 1
    if ($rule -and $rule.Group -ne 'MineHost') { throw 'A different application owns this rule name' }
    if ($r.action -eq 'firewall_get') {
      $value = @{present=[bool]$rule;name=$name;port='';profile='';remote='';protocol='';direction='';action='';enabled=$false}
      if ($rule) {
        $value.port = (($rule | Get-NetFirewallPortFilter).LocalPort -join ',')
        $value.protocol = [string]($rule | Get-NetFirewallPortFilter).Protocol
        $value.direction = [string]$rule.Direction
        $value.action = [string]$rule.Action
        $value.profile = [string]$rule.Profile
        $value.remote = (($rule | Get-NetFirewallAddressFilter).RemoteAddress -join ',')
        $value.enabled = ([string]$rule.Enabled -eq 'True')
      }
      ConvertTo-Json -InputObject $value -Compress
    } elseif ($r.action -eq 'firewall_remove') {
      if ($rule) { $rule | Remove-NetFirewallRule -ErrorAction Stop }
    } else {
      if ($r.profile -notin @('Private','Public') -or $r.remote -notin @('LocalSubnet','Any')) { throw 'Invalid firewall scope' }
      if ($rule) { throw 'Remove the existing MineHost rule before choosing a different scope' }
      New-NetFirewallRule -Name $name -DisplayName $name -Group 'MineHost' -Direction Inbound -Action Allow -Protocol TCP -LocalPort ([int]$r.port) -Profile $r.profile -RemoteAddress $r.remote -Enabled True -ErrorAction Stop | Out-Null
    }
  }
  { $_ -in @('upnp_get','upnp_add','upnp_remove') } {
    if ($r.id -notmatch '^[a-fA-F0-9-]{36}$' -or [int]$r.port -lt 1024 -or [int]$r.externalPort -lt 1024 -or [int]$r.externalPort -gt 65535) { throw 'Invalid mapping parameters' }
    $nat = New-Object -ComObject HNetCfg.NATUPnP
    $maps = $nat.StaticPortMappingCollection
    if ($null -eq $maps) { throw 'UPnP is unavailable or disabled on the router; use manual port forwarding or a tunnel' }
    $mapping = $null
    try { $mapping = $maps.Item([int]$r.externalPort,'TCP') } catch {}
    $description = 'MineHost-' + $r.id
    if ($r.action -eq 'upnp_get') {
      $value = @{present=[bool]$mapping;owned=$false;externalIp='';internalIp='';externalPort=[int]$r.externalPort;internalPort=0}
      if ($mapping) { $value.owned=($mapping.Description -eq $description);$value.externalIp=[string]$mapping.ExternalIPAddress;$value.internalIp=[string]$mapping.InternalClient;$value.internalPort=[int]$mapping.InternalPort }
      ConvertTo-Json -InputObject $value -Compress
    } elseif ($r.action -eq 'upnp_add') {
      if ($mapping) { throw 'This external port already has a mapping; it will not be overwritten' }
      $maps.Add([int]$r.externalPort,'TCP',[int]$r.port,[string]$r.internalIp,$true,$description) | Out-Null
    } else {
      if ($mapping) {
        if ($mapping.Description -ne $description -or [string]$mapping.InternalClient -ne [string]$r.internalIp -or [int]$mapping.InternalPort -ne [int]$r.port) { throw 'Mapping ownership or destination differs; refusing to remove it' }
        $maps.Remove([int]$r.externalPort,'TCP')
      }
    }
  }
  default { throw 'Unsupported network action' }
}

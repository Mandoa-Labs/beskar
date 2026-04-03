output "server_fqdn" {
  description = "Fully qualified domain name of the PostgreSQL server"
  value       = azurerm_postgresql_flexible_server.beskar.fqdn
}

output "database_name" {
  description = "Name of the database"
  value       = azurerm_postgresql_flexible_server_database.beskar.name
}

output "connection_string" {
  description = "PostgreSQL connection string (without password)"
  value       = "host=${azurerm_postgresql_flexible_server.beskar.fqdn} port=5432 user=${var.admin_username} dbname=${var.database_name} sslmode=require"
  sensitive   = true
}

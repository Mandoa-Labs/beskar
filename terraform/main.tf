terraform {
  required_providers {
    azurerm = {
      source  = "hashicorp/azurerm"
      version = "~> 4.0"
    }
  }
}

provider "azurerm" {
  features {}
  subscription_id = var.subscription_id
}

resource "azurerm_resource_group" "beskar" {
  name     = var.resource_group_name
  location = var.location
}

resource "azurerm_postgresql_flexible_server" "beskar" {
  name                          = var.server_name
  resource_group_name           = azurerm_resource_group.beskar.name
  location                      = azurerm_resource_group.beskar.location
  version                       = "16"
  administrator_login           = var.admin_username
  administrator_password        = var.admin_password
  storage_mb                    = var.storage_mb
  sku_name                      = var.sku_name
  zone                          = "1"
  public_network_access_enabled = true
}

resource "azurerm_postgresql_flexible_server_configuration" "pgvector" {
  server_id = azurerm_postgresql_flexible_server.beskar.id
  name      = "azure.extensions"
  value     = "VECTOR"
}

resource "azurerm_postgresql_flexible_server_firewall_rule" "allow_all" {
  name             = "allow-all"
  server_id        = azurerm_postgresql_flexible_server.beskar.id
  start_ip_address = "0.0.0.0"
  end_ip_address   = "255.255.255.255"
}

resource "azurerm_postgresql_flexible_server_database" "beskar" {
  name      = var.database_name
  server_id = azurerm_postgresql_flexible_server.beskar.id
  charset   = "UTF8"
  collation = "en_US.utf8"
}
